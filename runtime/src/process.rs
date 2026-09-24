//! One guest process: loading and starting a program (docs/abi.md,
//! "Module shape" and "Process startup"), and the state a process carries
//! across vfork and execve.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, bail};
use wasmtime::{Config, Engine, ExternType, Linker, Memory, Module, Store, Trap, Val};

use crate::abi::{PAGE_SIZE, at, errno::*};
use crate::fs::{FdTable, Fs};
use crate::kernel::{Kernel, Termination};
use crate::memory::{self as mem, AddressSpace, SysResult};

/// Width of masked addresses: 2^38 bytes (256 GB) per process.
const MASK_BITS: u32 = 38;

/// Number of signals, plus one so signal numbers can index directly.
pub const NSIG: usize = 65;
pub const SIG_IGN: u64 = 1;

/// A program ready to run: the result of resolving an execve.
pub struct ExecImage {
    /// The WebAssembly file on the host.
    pub host: PathBuf,
    /// The program's path inside the environment (AT_EXECFN).
    pub path: String,
    /// Raw bytes, as on Linux.
    pub argv: Vec<Vec<u8>>,
    pub env: Vec<Vec<u8>>,
}

/// What survives execve (and what a new process starts with).
pub struct Inherit {
    pub fds: FdTable,
    pub cwd: String,
    pub umask: u32,
    /// Signals set to SIG_IGN. Handlers reset to the default on exec;
    /// ignored signals stay ignored.
    pub ignored: u64,
}

impl Inherit {
    pub fn initial() -> Self {
        Inherit { fds: FdTable::with_stdio(), cwd: "/".into(), umask: 0o022, ignored: 0 }
    }
}

/// Parent state saved while a vfork child runs in the parent's memory.
pub struct VforkFrame {
    pub parent_pid: i32,
    pub fds: FdTable,
    pub cwd: String,
    pub umask: u32,
    pub sigactions: Vec<[u8; 32]>,
}

/// Something a syscall asks the dispatcher to do instead of returning.
pub enum Pending {
    Terminate(Termination),
    Exec(ExecImage),
}

/// Per-process state, owned by the wasmtime store.
pub struct Process {
    pub kernel: Arc<Kernel>,
    pub pid: i32,
    pub memory: Option<Memory>,
    pub space: AddressSpace,
    /// The program break. It never moves (docs/abi.md, "Memory").
    pub brk: u64,
    pub fds: FdTable,
    pub fs: Fs,
    pub umask: u32,
    /// The raw `struct k_sigaction` last set for each signal. No handler
    /// is ever run yet; this records dispositions (SIG_IGN) and reports
    /// them back to the program.
    pub sigactions: Vec<[u8; 32]>,
    pub vfork: Vec<VforkFrame>,
    pub pending: Option<Pending>,
    pub trace: bool,
    pub start: Instant,
}

impl Process {
    pub fn handler(&self, sig: usize) -> u64 {
        self.sigactions.get(sig).map_or(0, |a| u64::from_le_bytes(a[..8].try_into().unwrap()))
    }

    /// What a signal would do to this process: None if it's ignored
    /// (explicitly, by default, or because handlers can't run yet).
    pub fn signal_effect(&self, sig: i32) -> Option<Termination> {
        const SIGKILL: i32 = 9;
        // SIGCHLD, SIGCONT, SIGURG, SIGWINCH are ignored by default.
        const DEFAULT_IGNORED: [i32; 4] = [17, 18, 23, 28];
        if sig == SIGKILL {
            return Some(Termination::Signal(sig));
        }
        match self.handler(sig as usize) {
            0 if DEFAULT_IGNORED.contains(&sig) => None,
            0 => Some(Termination::Signal(sig)),
            SIG_IGN => None,
            _ => {
                if self.trace {
                    eprintln!("[wasm-env] pid {}: signal {sig} has a handler; handlers aren't supported yet", self.pid);
                }
                None
            }
        }
    }

    /// The state an execve'd program inherits from this process.
    pub fn inherit(&self) -> Inherit {
        let mut fds = self.fds.clone();
        fds.close_on_exec();
        let ignored = (1..NSIG).filter(|&s| self.handler(s) == SIG_IGN).fold(0u64, |m, s| m | 1 << s);
        Inherit { fds, cwd: self.fs.cwd.clone(), umask: self.umask, ignored }
    }
}

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
pub fn check_shape(module: &Module) -> Result<()> {
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

/// Resolves execve(path, argv, env) to a program, following `#!` lines.
pub fn resolve_exec(fs: &Fs, path: &str, mut argv: Vec<Vec<u8>>, env: Vec<Vec<u8>>) -> SysResult<ExecImage> {
    let mut path = path.to_string();
    for _ in 0..4 {
        let guest = fs.resolve(&fs.cwd, &path, true)?;
        let host = fs.host(&guest);
        let meta = std::fs::metadata(&host).map_err(|e| crate::fs::errno(&e))?;
        if !meta.is_file() {
            return Err(EACCES);
        }
        let mut head = [0u8; 256];
        let n = std::io::Read::read(&mut std::fs::File::open(&host).map_err(|e| crate::fs::errno(&e))?, &mut head)
            .map_err(|e| crate::fs::errno(&e))?;
        let head = &head[..n];
        if head.starts_with(b"\0asm") {
            return Ok(ExecImage { host, path: guest, argv, env });
        }
        let Some(line) = head.strip_prefix(b"#!") else {
            // Not WebAssembly: an ELF binary needs the translator (M2).
            return Err(ENOEXEC);
        };
        // "#!interpreter [one argument]", as Linux parses it.
        let line = line.split(|&b| b == b'\n').next().unwrap_or_default();
        let line = String::from_utf8_lossy(line).trim().to_string();
        let (interp, arg) = match line.split_once([' ', '\t']) {
            Some((i, a)) => (i.to_string(), Some(a.trim().to_string())),
            None => (line.clone(), None),
        };
        if interp.is_empty() {
            return Err(ENOEXEC);
        }
        let mut new_argv = vec![interp.clone().into_bytes()];
        new_argv.extend(arg.map(String::into_bytes));
        new_argv.push(path.clone().into_bytes());
        new_argv.extend(argv.into_iter().skip(1));
        argv = new_argv;
        path = interp;
    }
    Err(ELOOP)
}

pub enum Outcome {
    Done(Termination),
    Exec(ExecImage, Inherit),
}

/// Instantiates `image` as process `pid` and runs it until it exits or
/// execs another program.
pub fn run_image(kernel: &Arc<Kernel>, pid: i32, image: ExecImage, inherit: Inherit) -> Outcome {
    let module = match kernel.module(&image.host) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wasm-env: {}: {e}", image.path);
            return Outcome::Done(Termination::Exit(126));
        }
    };
    let mut sigactions = vec![[0u8; 32]; NSIG];
    for (sig, action) in sigactions.iter_mut().enumerate() {
        if inherit.ignored & (1 << sig) != 0 {
            action[..8].copy_from_slice(&SIG_IGN.to_le_bytes());
        }
    }
    let mut fs = Fs::new(kernel.root.clone());
    fs.cwd = inherit.cwd;
    let process = Process {
        kernel: kernel.clone(),
        pid,
        memory: None,
        space: AddressSpace::new(0),
        brk: 0,
        fds: inherit.fds,
        fs,
        umask: inherit.umask,
        sigactions,
        vfork: Vec::new(),
        pending: None,
        trace: kernel.trace,
        start: Instant::now(),
    };
    let mut store = Store::new(&kernel.engine, process);
    match start(&mut store, &module, &image) {
        Ok(()) => Outcome::Done(Termination::Exit(0)),
        Err(e) => match store.data_mut().pending.take() {
            Some(Pending::Exec(next)) => Outcome::Exec(next, store.data().inherit()),
            Some(Pending::Terminate(how)) => Outcome::Done(how),
            None => {
                // A trap ends the process like the matching Linux signal would.
                let sig = match e.downcast_ref::<Trap>() {
                    Some(Trap::MemoryOutOfBounds | Trap::StackOverflow) => 11, // SIGSEGV
                    Some(Trap::UnreachableCodeReached) => 6,                   // SIGABRT
                    Some(_) => 4,                                             // SIGILL
                    None => 6,
                };
                eprintln!("wasm-env: {} (pid {pid}): {e}", image.path);
                Outcome::Done(Termination::Signal(sig))
            }
        },
    }
}

/// The error a syscall returns to unwind out of the program (exit, fatal
/// signal or execve); the reason is in `Process::pending`.
#[derive(Debug)]
pub struct Unwind;

impl std::fmt::Display for Unwind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("process ended")
    }
}

impl std::error::Error for Unwind {}

fn start(store: &mut Store<Process>, module: &Module, image: &ExecImage) -> Result<()> {
    let mut linker = Linker::new(store.engine());
    linker.func_wrap("linux", "syscall", crate::syscall::syscall)?;
    let instance = linker.instantiate(&mut *store, module)?;
    let memory = instance.get_memory(&mut *store, "memory").unwrap();
    let heap_base = global_i64(instance.get_global(&mut *store, "__heap_base").unwrap().get(&mut *store))?;
    let stack_pointer = instance.get_global(&mut *store, "__stack_pointer").unwrap();
    let sp = global_i64(stack_pointer.get(&mut *store))?;

    let space = AddressSpace::new(heap_base);
    store.data_mut().brk = space.start;
    store.data_mut().space = space;
    store.data_mut().memory = Some(memory);

    let sp = write_initial_stack(memory.data_mut(&mut *store), sp, image)?;
    stack_pointer.set(&mut *store, Val::I64(sp as i64))?;

    let start = instance.get_typed_func::<i64, ()>(&mut *store, "_start")?;
    Ok(start.call(&mut *store, sp as i64)?)
}

fn global_i64(v: Val) -> Result<u64> {
    match v {
        Val::I64(v) => Ok(v as u64),
        _ => bail!("expected an i64 global"),
    }
}

/// Writes argc, argv, envp and the auxiliary vector below `top` in the
/// Linux layout, and returns the new stack pointer (16-byte aligned).
fn write_initial_stack(data: &mut [u8], top: u64, image: &ExecImage) -> Result<u64> {
    let fault = |_| anyhow::anyhow!("initial stack doesn't fit in memory");

    // Strings and AT_RANDOM bytes go at the very top.
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    let mut strings: Vec<u8> = Vec::new();
    let mut offsets = Vec::new();
    for s in image.argv.iter().chain(&image.env).map(Vec::as_slice).chain([image.path.as_bytes()]) {
        offsets.push(strings.len() as u64);
        strings.extend_from_slice(s);
        strings.push(0);
    }
    let random_off = strings.len() as u64;
    strings.extend_from_slice(&random);
    let strings_at = (top - strings.len() as u64) & !15;
    mem::write(data, strings_at, &strings).map_err(fault)?;

    let argc = image.argv.len();
    let envc = image.env.len();
    let auxv = [
        (at::PAGESZ, PAGE_SIZE),
        (at::RANDOM, strings_at + random_off),
        (at::UID, 0),
        (at::EUID, 0),
        (at::GID, 0),
        (at::EGID, 0),
        (at::SECURE, 0),
        (at::HWCAP, 0),
        (at::EXECFN, strings_at + offsets[argc + envc]),
        (at::NULL, 0),
    ];
    let mut words: Vec<u64> = vec![argc as u64];
    words.extend(offsets[..argc].iter().map(|o| strings_at + o));
    words.push(0);
    words.extend(offsets[argc..argc + envc].iter().map(|o| strings_at + o));
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
