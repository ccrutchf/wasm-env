//! Loading and starting a wasm-env program (docs/abi.md, "Module shape"
//! and "Process startup").

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
use wasmtime::{Config, Engine, ExternType, Linker, Memory, Module, Store, Trap, Val};

use crate::abi::{PAGE_SIZE, at};
use crate::fs::{FdTable, Fs};
use crate::memory::{self as mem, AddressSpace};

/// Width of masked addresses: 2^38 bytes (256 GB) per process.
const MASK_BITS: u32 = 38;

/// Per-process state, owned by the wasmtime store.
pub struct Process {
    pub memory: Option<Memory>,
    pub space: AddressSpace,
    /// The program break. It never moves (docs/abi.md, "Memory").
    pub brk: u64,
    pub fds: FdTable,
    pub fs: Fs,
    pub umask: u32,
    pub trace: bool,
    pub start: Instant,
}

/// Raised by exit_group (and fatal signals) to unwind out of the program.
#[derive(Debug)]
pub struct Exit(pub i32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit({})", self.0)
    }
}

impl std::error::Error for Exit {}

/// The engine for wasm-env programs: masked memory64 (see PLAN.md).
pub fn engine() -> Result<Engine> {
    // Our patched compiler reads the mask width from the environment.
    // SAFETY: called once at startup, before any other threads exist.
    unsafe { std::env::set_var("WASM_ENV_MASK_BITS", MASK_BITS.to_string()) };
    let mut config = Config::new();
    config
        .wasm_memory64(true)
        .wasm_exceptions(true)
        .wasm_tail_call(true)
        .cranelift_opt_level(wasmtime::OptLevel::Speed)
        .memory_reservation(1 << MASK_BITS)
        .memory_may_move(false)
        .memory_guard_size(64 << 20);
    if let Some(dir) = cache_dir() {
        let mut cache = wasmtime::CacheConfig::new();
        cache.with_directory(dir);
        config.cache(Some(wasmtime::Cache::new(cache)?));
    }
    Ok(Engine::new(&config)?)
}

/// Checks the module against docs/abi.md's "Module shape".
fn check_shape(module: &Module) -> Result<()> {
    for import in module.imports() {
        if (import.module(), import.name()) != ("linux", "syscall") {
            bail!("unsupported import {}.{} (wasm-env programs may only import linux.syscall)", import.module(), import.name());
        }
    }
    for (name, want) in [("memory", "memory"), ("_start", "func"), ("__stack_pointer", "global"), ("__heap_base", "global")] {
        let found = module.get_export(name).map(|t| match t {
            ExternType::Memory(m) if m.is_64() => "memory",
            ExternType::Func(_) => "func",
            ExternType::Global(_) => "global",
            _ => "other",
        });
        if found != Some(want) {
            bail!("missing export `{name}` (a 64-bit {want}); is this a wasm64-linux-musl program?");
        }
    }
    Ok(())
}

pub struct Program<'a> {
    pub wasm: &'a Path,
    pub root: PathBuf,
    /// argv[0] as the program sees it.
    pub argv: Vec<String>,
    pub env: Vec<String>,
    pub trace: bool,
}

/// Runs a program to completion and returns its exit status.
pub fn run(engine: &Engine, program: Program) -> Result<i32> {
    let module = Module::from_file(engine, program.wasm)
        .map_err(|e| anyhow::anyhow!("loading {}: {e:?}", program.wasm.display()))?;
    check_shape(&module)?;

    let mut linker = Linker::new(engine);
    linker.func_wrap("linux", "syscall", crate::syscall::syscall)?;

    let process = Process {
        memory: None,
        space: AddressSpace::new(0),
        brk: 0,
        fds: FdTable::with_stdio(),
        fs: Fs::new(program.root),
        umask: 0o022,
        trace: program.trace,
        start: Instant::now(),
    };
    let mut store = Store::new(engine, process);
    let instance = linker.instantiate(&mut store, &module)?;
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let heap_base = global_i64(instance.get_global(&mut store, "__heap_base").unwrap().get(&mut store))?;
    let stack_pointer = instance.get_global(&mut store, "__stack_pointer").unwrap();
    let sp = global_i64(stack_pointer.get(&mut store))?;

    let space = AddressSpace::new(heap_base);
    store.data_mut().brk = space.start;
    store.data_mut().space = space;
    store.data_mut().memory = Some(memory);

    let sp = write_initial_stack(memory.data_mut(&mut store), sp, &program.argv, &program.env)?;
    stack_pointer.set(&mut store, Val::I64(sp as i64))?;

    let start = instance.get_typed_func::<i64, ()>(&mut store, "_start")?;
    match start.call(&mut store, sp as i64) {
        Ok(()) => Ok(0),
        Err(e) => {
            if let Some(exit) = e.downcast_ref::<Exit>() {
                return Ok(exit.0);
            }
            // A trap ends the process like the matching Linux signal would.
            let status = match e.downcast_ref::<Trap>() {
                Some(Trap::MemoryOutOfBounds | Trap::StackOverflow) => 139, // SIGSEGV
                Some(Trap::UnreachableCodeReached) => 134,                   // SIGABRT
                Some(_) => 132,                                             // SIGILL
                None => return Err(anyhow::anyhow!("{e:?}")),
            };
            eprintln!("wasm-env: {}: {e}", program.wasm.display());
            Ok(status)
        }
    }
}

fn global_i64(v: Val) -> Result<u64> {
    match v {
        Val::I64(v) => Ok(v as u64),
        _ => bail!("expected an i64 global"),
    }
}

/// Writes argc, argv, envp and the auxiliary vector below `top` in the
/// Linux layout, and returns the new stack pointer (16-byte aligned).
fn write_initial_stack(data: &mut [u8], top: u64, argv: &[String], env: &[String]) -> Result<u64> {
    let fault = |_| anyhow::anyhow!("initial stack doesn't fit in memory");

    // Strings and AT_RANDOM bytes go at the very top.
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    let mut strings: Vec<u8> = Vec::new();
    let mut offsets = Vec::new();
    for s in argv.iter().chain(env) {
        offsets.push(strings.len() as u64);
        strings.extend_from_slice(s.as_bytes());
        strings.push(0);
    }
    let random_off = strings.len() as u64;
    strings.extend_from_slice(&random);
    let strings_at = (top - strings.len() as u64) & !15;
    mem::write(data, strings_at, &strings).map_err(fault)?;

    let execfn = strings_at; // argv[0]
    let auxv = [
        (at::PAGESZ, PAGE_SIZE),
        (at::RANDOM, strings_at + random_off),
        (at::UID, 0),
        (at::EUID, 0),
        (at::GID, 0),
        (at::EGID, 0),
        (at::SECURE, 0),
        (at::HWCAP, 0),
        (at::EXECFN, execfn),
        (at::NULL, 0),
    ];
    let mut words: Vec<u64> = vec![argv.len() as u64];
    words.extend(offsets[..argv.len()].iter().map(|o| strings_at + o));
    words.push(0);
    words.extend(offsets[argv.len()..].iter().map(|o| strings_at + o));
    words.push(0);
    for (k, v) in auxv {
        words.extend([k, v]);
    }
    let sp = (strings_at - words.len() as u64 * 8) & !15;
    for (i, w) in words.iter().enumerate() {
        mem::write_u64(data, sp + i as u64 * 8, *w).map_err(fault)?;
    }
    Ok(sp)
}

/// Where compiled programs are cached, so each .wasm is compiled once.
/// wasmtime's cache key covers its own settings but not our masking patch,
/// so the mask width is part of the directory name.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("WASM_ENV_CACHE").map(PathBuf::from).or_else(|| {
        let var = |k| std::env::var_os(k).map(PathBuf::from);
        if cfg!(windows) {
            var("LOCALAPPDATA")
        } else if cfg!(target_os = "macos") {
            var("HOME").map(|h| h.join("Library/Caches"))
        } else {
            var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
        }
        .map(|d| d.join("wasm-env"))
    })?;
    Some(base.join(format!("masked-{MASK_BITS}")))
}
