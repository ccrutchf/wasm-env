//! One guest process: loading and starting a program (docs/abi.md,
//! "Module shape" and "Process startup"), and the state a process carries
//! across vfork and execve.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, bail};
use wasmtime::{Config, Engine, ExternType, Linker, Memory, MemoryType, Module, SharedMemory, Store, Trap, Val};

use crate::abi::{PAGE_SIZE, at, errno::*};
use crate::fs::{FdTable, Fs};
use crate::kernel::{Kernel, Termination};
use crate::memory::{self as mem, AddressSpace, SysResult};
use crate::thread::{Group, GroupEnd};

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
    /// A translated program's dynamic linker (on the host), which the
    /// runtime loads and starts first, as the kernel would, when the
    /// program's libraries are modules of their own (docs/abi.md,
    /// "Libraries").
    pub interp: Option<PathBuf>,
}

/// What survives execve (and what a new process starts with).
pub struct Inherit {
    pub fds: FdTable,
    pub cwd: String,
    pub umask: u32,
    /// Signals set to SIG_IGN. Handlers reset to the default on exec;
    /// ignored signals stay ignored.
    pub ignored: u64,
    pub creds: Creds,
}

impl Inherit {
    pub fn initial() -> Self {
        Inherit { fds: FdTable::with_stdio(), cwd: "/".into(), umask: 0o022, ignored: 0, creds: Creds::default() }
    }
}

/// A process's user and group ids (real, effective, saved) and
/// supplementary groups. Everything starts as root. These are numbers the
/// program sees and changes (a daemon dropping privileges, apt running its
/// downloads as `_apt`); files keep belonging to the host user, and no
/// permissions are checked.
#[derive(Clone, Default)]
pub struct Creds {
    pub uid: [u32; 3],
    pub gid: [u32; 3],
    pub groups: Vec<u32>,
}

/// Parent state saved while a vfork child runs in the parent's memory.
pub struct VforkFrame {
    pub parent_pid: i32,
    pub fds: FdTable,
    pub cwd: String,
    pub umask: u32,
    pub sigactions: Vec<[u8; 32]>,
    pub creds: Creds,
}

/// Something a syscall asks the dispatcher to do instead of returning.
pub enum Pending {
    Terminate(Termination),
    Exec(ExecImage),
    /// This thread called exit() while others run.
    ThreadExit(i32),
    /// Another thread ended the process (see thread::Group).
    GroupEnded,
}

/// A program's memory: its own, or (a translated program, which may have
/// threads) a shared memory it imports (docs/abi.md, "Threads").
#[derive(Clone)]
pub enum Mem {
    Local(Memory),
    Shared(SharedMemory),
}

/// Per-thread state, owned by the thread's wasmtime store. What the
/// threads of a process share is in `group`, and `fds` is a shared table.
/// The working directory, umask and signal dispositions are per thread (a
/// new thread starts with its creator's).
pub struct Process {
    pub kernel: Arc<Kernel>,
    /// The process id (the thread group's), and this thread's id.
    pub pid: i32,
    pub tid: i32,
    pub memory: Option<Mem>,
    pub group: Arc<Group>,
    pub fds: FdTable,
    pub fs: Fs,
    pub umask: u32,
    /// The raw `struct k_sigaction` last set for each signal. No handler
    /// is ever run yet; this records dispositions (SIG_IGN) and reports
    /// them back to the program.
    pub sigactions: Vec<[u8; 32]>,
    pub creds: Creds,
    pub vfork: Vec<VforkFrame>,
    pub pending: Option<Pending>,
    pub trace: bool,
    pub start: Instant,
    /// The WebAssembly module this process runs (for translation feedback).
    pub module: PathBuf,
    /// Where to clear the thread's id when it exits (CLONE_CHILD_CLEARTID,
    /// set_tid_address), or 0.
    pub clear_tid: u64,
    /// For a thread being started: told once it has its registers (see
    /// thread::spawn).
    pub started: Option<std::sync::mpsc::Sender<()>>,
    /// This thread's main instance, when the program's libraries are modules
    /// of their own (see modules.rs).
    pub main: Option<crate::modules::Main>,
}

impl Process {
    pub fn handler(&self, sig: usize) -> u64 {
        self.sigactions.get(sig).map_or(0, |a| u64::from_le_bytes(a[..8].try_into().unwrap()))
    }

    /// What a signal would do to this process: None if it's ignored
    /// (explicitly, by default, or because handlers can't run yet).
    pub fn signal_effect(&self, sig: i32) -> Option<Termination> {
        let handler = self.handler(sig as usize);
        if self.trace && handler > SIG_IGN && sig != 9 {
            eprintln!("[wasm-env] pid {}: signal {sig} has a handler; handlers aren't supported yet", self.pid);
        }
        signal_effect(sig, handler)
    }

    /// The state an execve'd program inherits from this process.
    pub fn inherit(&self) -> Inherit {
        let fds = self.fds.copy();
        fds.close_on_exec();
        let ignored = (1..NSIG).filter(|&s| self.handler(s) == SIG_IGN).fold(0u64, |m, s| m | 1 << s);
        Inherit { fds, cwd: self.fs.cwd.clone(), umask: self.umask, ignored, creds: self.creds.clone() }
    }
}

/// What signal `sig` does to a process whose handler for it is `handler`
/// (0 = SIG_DFL, 1 = SIG_IGN, else a function that can't run yet).
pub fn signal_effect(sig: i32, handler: u64) -> Option<Termination> {
    const SIGKILL: i32 = 9;
    // SIGCHLD, SIGCONT, SIGURG, SIGWINCH are ignored by default.
    const DEFAULT_IGNORED: [i32; 4] = [17, 18, 23, 28];
    if sig == SIGKILL {
        return Some(Termination::Signal(sig));
    }
    match handler {
        0 if DEFAULT_IGNORED.contains(&sig) => None,
        0 => Some(Termination::Signal(sig)),
        _ => None,
    }
}

/// `WASM_ENV_DEADLINE`, in seconds, if set (see engine()).
fn deadline() -> Option<f64> {
    std::env::var("WASM_ENV_DEADLINE").ok()?.parse().ok()
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
        // Translated programs share their memory between threads (docs/abi.md,
        // "Threads").
        .wasm_threads(true)
        .shared_memory(true)
        // Relaxed SIMD's madd is how translated code gets a fused
        // multiply-add (arm64's FMADD, FMLA); deterministic mode makes it
        // fused on every host.
        .wasm_relaxed_simd(true)
        .relaxed_simd_deterministic(true)
        .cranelift_opt_level(wasmtime::OptLevel::Speed)
        .memory_reservation(1 << MASK_BITS)
        .memory_may_move(false)
        .memory_guard_size(64 << 20);
    // Debugging aid: WASM_ENV_DEADLINE=SECONDS stops programs still running
    // after that long, with a trap whose backtrace names the code running
    // (for a translated program, the block). Off by default: it costs checks.
    if deadline().is_some() {
        config.epoch_interruption(true);
    }
    // Profiling aid: WASM_ENV_PERFMAP=1 writes /tmp/perf-PID.map so `perf`
    // names the wasm functions (for a translated program, the regions);
    // WASM_ENV_PERFMAP=jitdump writes a jitdump (`perf record -k 1`, then
    // `perf inject --jit`) so `perf annotate` shows their instructions.
    match std::env::var("WASM_ENV_PERFMAP").as_deref() {
        Ok("jitdump") => {
            config.profiler(wasmtime::ProfilingStrategy::JitDump);
        }
        Ok(_) => {
            config.profiler(wasmtime::ProfilingStrategy::PerfMap);
        }
        Err(_) => {}
    }
    Ok(Engine::new(&config)?)
}

/// Loads the compiled program for a .wasm file, compiling it only the first
/// time. Compiled code is cached uncompressed and
/// mapped straight from the file, so a cached load of a large translated
/// program (hundreds of MB of wasm) takes milliseconds: wasmtime's own cache
/// hashes the whole .wasm and decompresses its entry on every load.
///
/// The entry is named by the .wasm's path and records its size, mtime and
/// the engine's settings; a .wasm that changes replaces its entry.
pub fn load_module(engine: &Engine, host: &std::path::Path) -> Result<Module> {
    use std::hash::{Hash, Hasher};
    let Some(dir) = cache_dir() else { return Ok(Module::from_file(engine, host)?) };
    let host = std::fs::canonicalize(host)?;
    let meta = std::fs::metadata(&host)?;
    let mtime = meta.modified()?.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut h);
    let stamp = format!("{} {} {} {:016x}\n", meta.len(), mtime.as_nanos(), MASK_BITS, h.finish());
    let key = crate::translate::hash(host.as_os_str().as_encoded_bytes());
    let (entry, stamp_file) = (dir.join(format!("{key:016x}.cwasm")), dir.join(format!("{key:016x}.stamp")));
    if std::fs::read_to_string(&stamp_file).is_ok_and(|s| s == stamp) {
        // SAFETY: the entry was written by `Module::serialize` below, for
        // this engine configuration (the stamp says so), and is replaced
        // only by rename, never modified in place.
        if let Ok(module) = unsafe { Module::deserialize_file(engine, &entry) } {
            return Ok(module);
        }
    }
    let module = Module::from_file(engine, &host)?;
    // Best effort: a cache that can't be written just means compiling again.
    let _ = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{key:016x}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, module.serialize().map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, &entry)?;
        let tmp = dir.join(format!("{key:016x}.{}.stamp.tmp", std::process::id()));
        std::fs::write(&tmp, &stamp)?;
        std::fs::rename(&tmp, &stamp_file)
    })();
    Ok(module)
}

/// Checks the module against docs/abi.md's "Module shape".
pub fn check_shape(module: &Module) -> Result<()> {
    for import in module.imports() {
        match (import.module(), import.name()) {
            ("linux", "syscall") => {}
            ("env", "memory") if shared_memory_type(module).is_some() => {}
            _ => bail!(
                "unsupported import {}.{} (wasm-env programs may only import linux.syscall, and a shared memory)",
                import.module(),
                import.name()
            ),
        }
    }
    let mut required = vec![("_start", "func"), ("__stack_pointer", "global"), ("__heap_base", "global")];
    if shared_memory_type(module).is_none() {
        required.push(("memory", "memory"));
    }
    for (name, want) in required {
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

/// The type of the shared 64-bit memory a module imports as `env.memory`,
/// if it does (a translated program; docs/abi.md, "Threads").
pub fn shared_memory_type(module: &Module) -> Option<MemoryType> {
    module.imports().find_map(|i| match i.ty() {
        ExternType::Memory(m) if (i.module(), i.name()) == ("env", "memory") && m.is_shared() && m.is_64() => Some(m),
        _ => None,
    })
}

/// Resolves execve(path, argv, env) to a program, following `#!` lines and
/// translating arm64 ELF binaries on first use (`trace` logs translation).
pub fn resolve_exec(fs: &Fs, path: &str, mut argv: Vec<Vec<u8>>, env: Vec<Vec<u8>>, trace: bool) -> SysResult<ExecImage> {
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
            return Ok(ExecImage { host, path: guest, argv, env, interp: None });
        }
        if crate::translate::is_arm64_elf(head) {
            // Translate the arm64 binary to WebAssembly; it targets the same
            // ABI, so it runs like any other program. argv[0] stays the
            // guest path.
            let name = guest.rsplit('/').next().unwrap_or_default();
            let wasm = crate::translate::ensure_translated(&host, &fs.host("/"), name, trace)?;
            let interp = interpreter(fs, &host);
            return Ok(ExecImage { host: wasm, path: guest, argv, env, interp });
        }
        let Some(line) = head.strip_prefix(b"#!") else {
            // Not WebAssembly, not a shebang, not an arm64 ELF.
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

/// Whether an ELF file (its first bytes, through the program headers) has
/// a PT_INTERP: a dynamically-linked executable.
pub fn has_interpreter(bytes: &[u8]) -> bool {
    let word = |at: usize, n: usize| -> Option<u64> {
        let b = bytes.get(at..at + n)?;
        Some(b.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64))
    };
    let headers = || -> Option<bool> {
        let (phoff, phentsize, phnum) = (word(32, 8)? as usize, word(54, 2)? as usize, word(56, 2)? as usize);
        Some((0..phnum).any(|k| word(phoff + k * phentsize, 4) == Some(3)))
    };
    headers().unwrap_or(false)
}

/// The dynamic linker (PT_INTERP) of the arm64 executable at `elf`, found
/// in the environment's root, when the program's libraries are modules of
/// their own; None for a static executable or a monolithic translation.
pub fn interpreter(fs: &Fs, elf: &std::path::Path) -> Option<PathBuf> {
    if crate::translate::monolithic() {
        return None;
    }
    let bytes = std::fs::read(elf).ok()?;
    let word = |at: usize, n: usize| -> Option<u64> {
        let b = bytes.get(at..at + n)?;
        Some(b.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64))
    };
    let (phoff, phentsize, phnum) = (word(32, 8)? as usize, word(54, 2)? as usize, word(56, 2)? as usize);
    for k in 0..phnum {
        let ph = phoff + k * phentsize;
        if word(ph, 4)? == 3 {
            // PT_INTERP: a NUL-terminated path
            let (off, size) = (word(ph + 8, 8)? as usize, word(ph + 32, 8)? as usize);
            let raw = bytes.get(off..off + size)?;
            let path = std::str::from_utf8(raw.split(|&b| b == 0).next()?).ok()?;
            let guest = fs.resolve("/", path, true).ok()?;
            return Some(fs.host(&guest));
        }
    }
    None
}

/// Loads a program's dynamic linker as the kernel does: its segments at the
/// base its translation expects (a library module of its own, see
/// modules.rs). Returns the auxiliary vector entries that tell the program
/// where it is (AT_BASE) and to start there (the support code's
/// WASM_ENV_AT_START).
fn load_interpreter(store: &mut Store<Process>, path: &std::path::Path) -> Result<Vec<(u64, u64)>> {
    let lib = crate::modules::load(&mut *store, path)?;
    let bytes = std::fs::read(path)?;
    let word = |at: usize, n: usize| -> Result<u64> {
        let b = bytes.get(at..at + n).ok_or_else(|| anyhow::anyhow!("{}: truncated ELF", path.display()))?;
        Ok(b.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64))
    };
    let data = crate::syscall::memory_bytes(store);
    let (phoff, phentsize, phnum) = (word(32, 8)? as usize, word(54, 2)? as usize, word(56, 2)? as usize);
    for k in 0..phnum {
        let ph = phoff + k * phentsize;
        if word(ph, 4)? != 1 {
            continue; // PT_LOAD only
        }
        let (off, vaddr, filesz, memsz) = (word(ph + 8, 8)?, word(ph + 16, 8)?, word(ph + 32, 8)?, word(ph + 40, 8)?);
        let file = bytes.get(off as usize..(off + filesz) as usize).ok_or_else(|| anyhow::anyhow!("bad segment"))?;
        let at = lib.base + vaddr;
        mem::write(data, at, file).map_err(|e| anyhow::anyhow!("errno {e}"))?;
        mem::slice_mut(data, at + filesz, memsz - filesz).map_err(|e| anyhow::anyhow!("errno {e}"))?.fill(0);
    }
    const WASM_ENV_AT_START: u64 = 0x7700;
    Ok(vec![(at::BASE, lib.base), (WASM_ENV_AT_START, lib.base + word(24, 8)?)])
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
    let group = Group::new();
    let process = Process {
        kernel: kernel.clone(),
        pid,
        tid: pid,
        memory: None,
        group: group.clone(),
        fds: inherit.fds,
        fs,
        umask: inherit.umask,
        creds: inherit.creds,
        sigactions,
        vfork: Vec::new(),
        pending: None,
        trace: kernel.trace,
        start: Instant::now(),
        module: image.host.clone(),
        clear_tid: 0,
        started: None,
        main: None,
    };
    let mut store = Store::new(&kernel.engine, process);
    set_deadline(&mut store);
    let result = start(&mut store, &module, &image);
    finish(&mut store, result, &image.path)
}

/// How a process ends once its first thread stops (with `result`): as that
/// thread says, or, if another thread ended the process, as that one says.
/// `name` names the program in error messages.
pub fn finish(store: &mut Store<Process>, result: Result<()>, name: &str) -> Outcome {
    let p = store.data_mut();
    let (group, pid) = (p.group.clone(), p.pid);
    let from_group = |end: Option<GroupEnd>| match end {
        Some(GroupEnd::Exec(next, inherit)) => Outcome::Exec(next, inherit),
        Some(GroupEnd::Terminate(how)) => Outcome::Done(how),
        None => Outcome::Done(Termination::Exit(0)),
    };
    let outcome = match (p.pending.take(), result) {
        (None, Ok(())) => Outcome::Done(Termination::Exit(0)),
        (Some(Pending::Exec(next)), _) => Outcome::Exec(next, p.inherit()),
        (Some(Pending::Terminate(how)), _) => Outcome::Done(how),
        (Some(Pending::GroupEnded), _) => from_group(group.take_end()),
        (Some(Pending::ThreadExit(code)), _) => {
            // exit() in the first thread: the process runs on until its
            // last thread exits.
            crate::thread::exit_thread(p);
            group.thread_exited(Some(code));
            return from_group(group.wait_end());
        }
        (None, Err(e)) => {
            // A trap ends the process like the matching Linux signal would.
            eprintln!("wasm-env: {name} (pid {pid}): {e}");
            Outcome::Done(Termination::Signal(trap_signal(&e)))
        }
    };
    // Any other threads stop at their next syscall.
    group.end(GroupEnd::Terminate(Termination::Exit(0)));
    outcome
}

/// The signal a trap corresponds to.
pub fn trap_signal(e: &anyhow::Error) -> i32 {
    match e.downcast_ref::<Trap>() {
        Some(Trap::MemoryOutOfBounds | Trap::StackOverflow) => 11, // SIGSEGV
        Some(Trap::UnreachableCodeReached) => 6,                   // SIGABRT
        Some(_) => 4,                                             // SIGILL
        None => 6,
    }
}

/// WASM_ENV_DEADLINE (see engine()) for a new store.
pub fn set_deadline(store: &mut Store<Process>) {
    if let Some(seconds) = deadline() {
        store.set_epoch_deadline(1);
        let engine = store.engine().clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
            engine.increment_epoch();
        });
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

/// Instantiates `module` in `store`, on `shared` if the module imports its
/// memory (a translated program), and records the memory in the process.
pub fn instantiate(store: &mut Store<Process>, module: &Module, shared: Option<&SharedMemory>) -> Result<wasmtime::Instance> {
    let mut linker = Linker::new(store.engine());
    linker.func_wrap("linux", "syscall", crate::syscall::syscall)?;
    if let Some(memory) = shared {
        linker.define(&*store, "env", "memory", memory.clone())?;
    }
    let instance = linker.instantiate(&mut *store, module)?;
    let memory = match shared {
        Some(m) => Mem::Shared(m.clone()),
        None => Mem::Local(instance.get_memory(&mut *store, "memory").unwrap()),
    };
    store.data_mut().memory = Some(memory);
    Ok(instance)
}

fn start(store: &mut Store<Process>, module: &Module, image: &ExecImage) -> Result<()> {
    // A translated program's memory is shared, so its threads can use it,
    // and the runtime maps the guest image into it (see image.rs).
    let shared = match shared_memory_type(module) {
        Some(ty) => {
            let memory = SharedMemory::new(store.engine(), ty)?;
            crate::image::map(&memory, &image.host)?;
            Some(memory)
        }
        None => None,
    };
    let instance = instantiate(store, module, shared.as_ref())?;
    store.data_mut().main = crate::modules::Main::new(&mut *store, instance);
    if let Some(memory) = shared {
        let _ = store.data().group.threading.set(crate::thread::Threading { module: module.clone(), memory });
    }
    let heap_base = global_i64(instance.get_global(&mut *store, "__heap_base").unwrap().get(&mut *store))?;
    let stack_pointer = instance.get_global(&mut *store, "__stack_pointer").unwrap();
    let sp = global_i64(stack_pointer.get(&mut *store))?;

    // The program break starts at the heap base and may grow by up to
    // mem::BRK_MAX; mmap allocations start above that (docs/abi.md, "Memory").
    let brk = mem::page_align(heap_base);
    *store.data().group.mm.lock().unwrap() =
        crate::thread::Mm { space: AddressSpace::new(brk + mem::BRK_MAX), brk, brk_start: brk };
    // A dynamically-linked program whose libraries are modules of their own
    // starts in its dynamic linker, which the runtime loads first.
    let extra = match &image.interp {
        Some(path) if store.data().main.is_some() => load_interpreter(store, path)?,
        _ => Vec::new(),
    };

    let sp = write_initial_stack(crate::syscall::memory_bytes(store), sp, image, &extra)?;
    stack_pointer.set(&mut *store, Val::I64(sp as i64))?;

    let start = instance.get_typed_func::<i64, ()>(&mut *store, "_start")?;
    let result = start.call(&mut *store, sp as i64);
    if result.is_err() {
        recent_blocks(store, &instance);
    }
    Ok(result?)
}

/// For a translated program that stopped with a trap (not an exit): prints
/// the guest blocks it ran last, which the support code records in a ring
/// buffer (support/runtime.c), when it exports one.
fn recent_blocks(store: &mut Store<Process>, instance: &wasmtime::Instance) {
    if store.data().pending.is_some() {
        return; // exit or exec, not a fault
    }
    let (Some(buf), Some(count)) =
        (instance.get_global(&mut *store, "wasm_env_recent"), instance.get_global(&mut *store, "wasm_env_recent_count"))
    else {
        return;
    };
    let (Ok(buf), Ok(count)) = (global_i64(buf.get(&mut *store)), global_i64(count.get(&mut *store))) else { return };
    let data = crate::syscall::memory_bytes(store);
    let word = |a: u64| data.get(a as usize..a as usize + 8).map(|w| u64::from_le_bytes(w.try_into().unwrap()));
    let Some(n) = word(count) else { return };
    eprintln!("wasm-env: the translated program's most recent blocks, oldest first:");
    for k in n.saturating_sub(16)..n {
        if let Some(a) = word(buf + 8 * (k % 64)) {
            eprintln!("  {a:#x}");
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
fn write_initial_stack(data: &mut [u8], top: u64, image: &ExecImage, extra: &[(u64, u64)]) -> Result<u64> {
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
    let strings_at = top.checked_sub(strings.len() as u64).ok_or_else(|| fault(EFAULT))? & !15;
    mem::write(data, strings_at, &strings).map_err(fault)?;

    let argc = image.argv.len();
    let envc = image.env.len();
    let mut auxv = vec![
        (at::PAGESZ, PAGE_SIZE),
        (at::RANDOM, strings_at + random_off),
        (at::UID, 0),
        (at::EUID, 0),
        (at::GID, 0),
        (at::EGID, 0),
        (at::SECURE, 0),
        (at::HWCAP, 0),
        (at::EXECFN, strings_at + offsets[argc + envc]),
    ];
    auxv.extend_from_slice(extra);
    auxv.push((at::NULL, 0));
    let mut words: Vec<u64> = vec![argc as u64];
    words.extend(offsets[..argc].iter().map(|o| strings_at + o));
    words.push(0);
    words.extend(offsets[argc..argc + envc].iter().map(|o| strings_at + o));
    words.push(0);
    for (k, v) in auxv {
        words.extend([k, v]);
    }
    let sp = strings_at.checked_sub(words.len() as u64 * 8).ok_or_else(|| fault(EFAULT))? & !15;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch root directory, removed when dropped.
    struct Root(PathBuf);

    impl Root {
        fn new(name: &str) -> Root {
            let dir = std::env::temp_dir().join(format!("wasm-env-process-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            Root(dir)
        }

        fn file(&self, path: &str, contents: &[u8]) {
            std::fs::write(self.0.join(path.trim_start_matches('/')), contents).unwrap();
        }

        fn fs(&self) -> Fs {
            Fs::new(self.0.clone())
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn args(a: &[&str]) -> Vec<Vec<u8>> {
        a.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn exec_resolves_a_wasm_program() {
        let root = Root::new("wasm");
        root.file("/bin/prog", b"\0asm\x01\0\0\0");
        let image = resolve_exec(&root.fs(), "/bin/prog", args(&["prog", "x"]), args(&["A=1"]), false).unwrap();
        assert_eq!(image.path, "/bin/prog");
        assert_eq!(image.host, root.0.join("bin/prog"));
        assert_eq!(image.argv, args(&["prog", "x"]));
        assert_eq!(image.env, args(&["A=1"]));
    }

    #[test]
    fn exec_follows_shebang_lines() {
        let root = Root::new("shebang");
        root.file("/bin/sh", b"\0asm");
        root.file("/bin/script", b"#!/bin/sh -e\necho hi\n");
        let image = resolve_exec(&root.fs(), "/bin/script", args(&["script", "a", "b"]), vec![], false).unwrap();
        assert_eq!(image.path, "/bin/sh");
        assert_eq!(image.argv, args(&["/bin/sh", "-e", "/bin/script", "a", "b"]), "Linux shebang argv");
    }

    #[test]
    fn exec_follows_nested_interpreters() {
        let root = Root::new("nested");
        root.file("/bin/sh", b"\0asm");
        root.file("/bin/interp", b"#!/bin/sh\n");
        root.file("/bin/script", b"#!/bin/interp\n");
        let image = resolve_exec(&root.fs(), "/bin/script", args(&["script"]), vec![], false).unwrap();
        assert_eq!(image.argv, args(&["/bin/sh", "/bin/interp", "/bin/script"]));
    }

    #[test]
    fn exec_errors() {
        let root = Root::new("errors");
        root.file("/bin/elf", b"\x7fELF\x02\x01\x01");
        root.file("/bin/loop", b"#!/bin/loop\n");
        let fs = root.fs();
        let exec = |p: &str| resolve_exec(&fs, p, args(&["x"]), vec![], false).map(|_| ());
        assert_eq!(exec("/bin/missing"), Err(ENOENT));
        assert_eq!(exec("/bin"), Err(EACCES), "directories aren't executable");
        assert_eq!(exec("/bin/elf"), Err(ENOEXEC), "native binaries need the translator");
        assert_eq!(exec("/bin/loop"), Err(ELOOP), "interpreter chains are limited");
    }

    fn u64_at(data: &[u8], addr: u64) -> u64 {
        u64::from_le_bytes(data[addr as usize..addr as usize + 8].try_into().unwrap())
    }

    fn cstr_at(data: &[u8], addr: u64) -> &[u8] {
        let s = &data[addr as usize..];
        &s[..s.iter().position(|&b| b == 0).unwrap()]
    }

    #[test]
    fn initial_stack_has_the_linux_layout() {
        let mut data = vec![0u8; 8192];
        let image = ExecImage {
            host: PathBuf::new(),
            path: "/bin/prog".into(),
            argv: args(&["prog", "arg with spaces"]),
            env: vec![b"A=1".to_vec(), vec![0xff, b'=', 0x80]], // not UTF-8
            interp: None,
        };
        let sp = write_initial_stack(&mut data, 8000, &image, &[]).unwrap();
        assert_eq!(sp % 16, 0, "16-byte aligned");
        assert!(sp < 8000);

        assert_eq!(u64_at(&data, sp), 2, "argc");
        let argv = |i: u64| u64_at(&data, sp + 8 + i * 8);
        assert_eq!(cstr_at(&data, argv(0)), b"prog");
        assert_eq!(cstr_at(&data, argv(1)), b"arg with spaces");
        assert_eq!(argv(2), 0, "argv ends with NULL");
        let env = |i: u64| u64_at(&data, sp + 32 + i * 8);
        assert_eq!(cstr_at(&data, env(0)), b"A=1");
        assert_eq!(cstr_at(&data, env(1)), [0xff, b'=', 0x80], "raw bytes survive");
        assert_eq!(env(2), 0, "envp ends with NULL");

        let mut aux = std::collections::HashMap::new();
        let mut p = sp + 56;
        loop {
            let (k, v) = (u64_at(&data, p), u64_at(&data, p + 8));
            if k == at::NULL {
                break;
            }
            aux.insert(k, v);
            p += 16;
        }
        assert_eq!(aux[&at::PAGESZ], 4096);
        assert_eq!(cstr_at(&data, aux[&at::EXECFN]), b"/bin/prog");
        assert_eq!(aux[&at::UID], 0);
        let random = aux[&at::RANDOM];
        assert!(random + 16 <= 8000, "AT_RANDOM bytes are inside the block");
    }

    #[test]
    fn initial_stack_that_doesnt_fit_is_an_error() {
        let mut data = vec![0u8; 64];
        let image = ExecImage { host: PathBuf::new(), path: "/p".into(), argv: args(&["p"]), env: vec![], interp: None };
        assert!(write_initial_stack(&mut data, 64, &image, &[]).is_err());
    }

    #[test]
    fn signal_dispositions() {
        use Termination::Signal;
        assert_eq!(signal_effect(15, 0), Some(Signal(15)), "SIGTERM default: terminate");
        assert_eq!(signal_effect(15, SIG_IGN), None, "ignored");
        assert_eq!(signal_effect(17, 0), None, "SIGCHLD is ignored by default");
        assert_eq!(signal_effect(28, 0), None, "SIGWINCH is ignored by default");
        assert_eq!(signal_effect(9, SIG_IGN), Some(Signal(9)), "SIGKILL can't be ignored");
        assert_eq!(signal_effect(2, 0x1234), None, "handlers can't run yet, so the signal is dropped");
    }
}
