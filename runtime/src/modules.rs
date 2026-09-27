//! Libraries translated as modules of their own (docs/abi.md, "Libraries").
//!
//! A translated program whose libraries are separate is a main module (the
//! executable, with the translator's support code) plus a position-
//! independent side module per shared library, in the dynamic-linking
//! convention wasm-ld follows (`dylink.0`). When the guest's dynamic linker
//! maps a library, the support code asks the runtime to load it: the
//! runtime translates the library if it hasn't yet (for the fixed guest
//! base it keeps for that library), chooses where the module's own data
//! and function-table entries go in this process, instantiates it against
//! the main instance's exports, and adds it to the directory the support
//! code's `dispatch` searches. Every thread has its own instances (its own
//! store), created the same way with the same placements; a thread catches
//! up on libraries loaded since it last looked (`sync`).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use wasmtime::{AsContextMut, Extern, Global, GlobalType, Instance, Linker, Module, Mutability, Ref, SharedMemory, Table, Val, ValType};

use crate::process::Process;

/// A library loaded into a process: the same for each of its threads.
pub struct Library {
    /// The shared object on the host, and its translated module.
    pub path: PathBuf,
    pub wasm: PathBuf,
    pub module: Module,
    /// The guest addresses it was translated for.
    pub base: u64,
    pub end: u64,
    /// Where its module's data lives, and its function-table entries: one
    /// slot per `GOT.func` import, in import order, from `table_base`.
    memory_base: u64,
    table_base: u64,
    got: Vec<String>,
    /// Code the program generated at run time (see translate_code), not a
    /// shared object; `path` just names it.
    pub code: bool,
    /// Its entry in the module directory, in guest memory (see register).
    entry: u64,
    /// Retired: its code was unmapped or rewritten (see retire_code).
    dead: std::sync::atomic::AtomicBool,
}

/// The libraries a process has loaded, in load order.
#[derive(Default)]
pub struct Libraries {
    pub list: Mutex<Vec<Arc<Library>>>,
    /// The next free function-table slot (after the main module's).
    next_slot: Mutex<u64>,
    /// The address of the support code's `__wasm_env_code_epoch`, once a
    /// module is loaded.
    epoch: OnceLock<u64>,
}

impl Libraries {
    /// The library covering a guest address, if any (the newest, and not a
    /// retired one).
    pub fn at(&self, addr: u64) -> Option<Arc<Library>> {
        self.list.lock().unwrap().iter().rev().find(|l| addr >= l.base && addr < l.end && !l.dead.load(Ordering::Relaxed)).cloned()
    }
}

/// A thread's main instance, as its library instances link against it.
#[derive(Clone)]
pub struct Main {
    pub instance: Instance,
    table: Table,
    /// Library instances this store has.
    pub loaded: usize,
}

impl Main {
    /// Records the main instance of a store, if it's a program whose
    /// libraries are separate (it exports the module directory).
    pub fn new(store: impl AsContextMut<Data = Process>, instance: Instance) -> Option<Main> {
        let mut store = store;
        instance.get_export(&mut store, "__wasm_env_modules")?;
        let table = instance.get_table(&mut store, "__indirect_function_table")?;
        Some(Main { instance, table, loaded: 0 })
    }
}

/// A side module's memory and table needs (its `dylink.0` section).
struct DylinkInfo {
    mem_size: u64,
    mem_align: u64,
    table_size: u64,
}

/// Reads the `dylink.0` section (the first section of a side module).
fn dylink(wasm: &Path) -> Result<DylinkInfo> {
    use std::io::Read;
    let mut head = vec![0u8; 256];
    let n = std::fs::File::open(wasm)?.read(&mut head)?;
    head.truncate(n);
    let uleb = |b: &[u8], p: &mut usize| -> Result<u64> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let byte = *b.get(*p).context("truncated dylink.0")?;
            *p += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    };
    let mut p = 8;
    if head.get(p) != Some(&0) {
        bail!("{}: not a side module (no dylink.0 section)", wasm.display());
    }
    p += 1;
    let size = uleb(&head, &mut p)? as usize;
    let end = p + size;
    let name_len = uleb(&head, &mut p)? as usize;
    if head.get(p..p + name_len) != Some(b"dylink.0".as_slice()) {
        bail!("{}: not a side module (no dylink.0 section)", wasm.display());
    }
    p += name_len;
    while p < end.min(head.len()) {
        let kind = head[p];
        p += 1;
        let len = uleb(&head, &mut p)? as usize;
        if kind == 1 {
            // WASM_DYLINK_MEM_INFO
            let mut q = p;
            let mem_size = uleb(&head, &mut q)?;
            let mem_align = uleb(&head, &mut q)?;
            let table_size = uleb(&head, &mut q)?;
            return Ok(DylinkInfo { mem_size, mem_align: 1 << mem_align, table_size });
        }
        p += len;
    }
    Ok(DylinkInfo { mem_size: 0, mem_align: 1, table_size: 0 })
}

/// Loads the library in `path` (a host file) into the process, translating
/// it first if needed, and instantiates it in this thread's store. Returns
/// it (with its guest range); loading one already loaded returns that.
pub fn load(mut store: impl AsContextMut<Data = Process>, path: &Path) -> Result<Arc<Library>> {
    let process = store.as_context().data();
    let (kernel, group, trace) = (process.kernel.clone(), process.group.clone(), process.trace);
    let libraries = &group.libraries;
    let path = std::fs::canonicalize(path)?;
    // One library loads at a time (glibc's dynamic linker holds a lock too).
    let mut next_slot = libraries.next_slot.lock().unwrap();
    // This store must have every library loaded so far before another.
    sync(&mut store)?;
    if let Some(lib) = libraries.list.lock().unwrap().iter().find(|l| l.path == path) {
        return Ok(lib.clone());
    }
    let (wasm, base, end) = crate::translate::ensure_library(&path, &kernel.root, trace).map_err(|e| anyhow::anyhow!("translating {}: errno {e}", path.display()))?;
    add(&mut store, &mut next_slot, path, wasm, base, end, false)
}

/// Adds a translated module covering guest [base, end) to the process and
/// instantiates it in this thread's store (the caller holds `next_slot`
/// and has synced the store).
fn add(
    mut store: impl AsContextMut<Data = Process>,
    next_slot: &mut u64,
    path: PathBuf,
    wasm: PathBuf,
    base: u64,
    end: u64,
    code: bool,
) -> Result<Arc<Library>> {
    let process = store.as_context().data();
    let (kernel, group) = (process.kernel.clone(), process.group.clone());
    let threading = group.threading.get().context("not a translated program")?;
    let libraries = &group.libraries;
    let module = kernel.module_any(&wasm).map_err(anyhow::Error::msg)?;
    let info = dylink(&wasm)?;
    // The module's data goes in the process's mmap region.
    let mem_len = crate::memory::page_align(info.mem_size.max(1) + info.mem_align);
    let region = crate::syscall::alloc_shared(&group, &threading.memory, mem_len).map_err(|e| anyhow::anyhow!("errno {e}"))?;
    let memory_base = region.next_multiple_of(info.mem_align);
    let got: Vec<String> = module
        .imports()
        .filter(|i| i.module() == "GOT.func")
        .map(|i| i.name().to_string())
        .collect();
    let main = store.as_context().data().main.clone().context("no main instance")?;
    if *next_slot == 0 {
        *next_slot = main.table.size(&store);
    }
    let table_base = *next_slot;
    *next_slot += info.table_size + got.len() as u64;
    let k = libraries.list.lock().unwrap().len();
    const MODULES: usize = 1024;
    if k >= MODULES {
        bail!("too many libraries");
    }
    let entry = global(&mut store, &main.instance, "__wasm_env_modules")? + 40 * k as u64;
    if libraries.epoch.get().is_none() {
        let _ = libraries.epoch.set(global(&mut store, &main.instance, "__wasm_env_code_epoch")?);
    }
    let lib = Arc::new(Library { path, wasm, module, base, end, memory_base, table_base, got, code, entry, dead: false.into() });
    // The guest range: the dynamic linker maps a library there.
    crate::syscall::grow_shared(&threading.memory, end).map_err(|e| anyhow::anyhow!("errno {e}"))?;
    let instance = instantiate(&mut store, &lib, &threading.memory)?;
    // Relocate its data (function pointers become table slots) once.
    if let Some(f) = instance.get_func(&mut store, "__wasm_apply_data_relocs") {
        f.typed::<(), ()>(&store)?.call(&mut store, ())?;
    }
    register(&mut store, &instance, &lib, &threading.memory, k)?;
    libraries.list.lock().unwrap().push(lib.clone());
    if let Some(main) = store.as_context_mut().data_mut().main.as_mut() {
        main.loaded += 1;
    }
    Ok(lib)
}

/// A global's value (an i64) in an instance.
fn global(mut store: impl AsContextMut, inst: &Instance, name: &str) -> Result<u64> {
    match inst.get_global(&mut store, name).map(|g| g.get(&mut store)) {
        Some(Val::I64(v)) => Ok(v as u64),
        _ => bail!("missing export {name}"),
    }
}

/// Code the program generated at run time (a JIT's), reached at `pc`: the
/// wasm-env private call behind the support code's `translate_code`.
/// Translates the executable mapping holding `pc` (as it is now) and adds
/// it to the process. For an entry into code already translated that
/// wasn't one of its blocks, translates that code again with `pc` as a
/// root, retiring the old module. ENOEXEC if `pc` isn't in such memory (a
/// library's missed target, say) or this entry was tried already.
pub fn translate_code(mut store: impl AsContextMut<Data = Process>, pc: u64) -> crate::memory::SysResult<i64> {
    use crate::abi::errno::*;
    let process = store.as_context().data();
    let (kernel, group, trace) = (process.kernel.clone(), process.group.clone(), process.trace);
    let threading = group.threading.get().ok_or(ENOEXEC)?;
    let libraries = &group.libraries;
    let mut next_slot = libraries.next_slot.lock().unwrap();
    sync(&mut store).map_err(|_| EIO)?;
    let old = libraries.at(pc);
    let (lo, hi) = match &old {
        Some(lib) if lib.code => (lib.base, lib.end),
        Some(_) => return Err(ENOEXEC),
        None => group.mm.lock().unwrap().code_at(pc).ok_or(ENOEXEC)?,
    };
    const MAX_CODE: u64 = 256 << 20;
    if hi - lo > MAX_CODE {
        return Err(ENOEXEC);
    }
    let bytes = crate::memory::slice(crate::syscall::shared_bytes(&threading.memory), lo, hi - lo)?.to_vec();
    let (wasm, new) = crate::translate::ensure_code(&bytes, lo, pc, &kernel.root, trace)?;
    if old.is_some() && !new {
        return Err(ENOEXEC);
    }
    if trace {
        eprintln!("[wasm-env] translated code at {lo:#x}..{hi:#x} (entered at {pc:#x})");
    }
    if let Some(old) = &old {
        retire(old, &threading.memory, libraries);
    }
    let name = PathBuf::from(format!("[code {lo:#x}]"));
    add(&mut store, &mut next_slot, name, wasm, lo, hi, true).map_err(|e| {
        eprintln!("wasm-env: loading code translated from {lo:#x}: {e:#}");
        EIO
    })?;
    Ok(0)
}

/// Retires translated code: empties its directory entry, so dispatch no
/// longer finds it, and bumps the code epoch, so every thread clears its
/// dispatch cache (which may name its regions) before it next dispatches.
fn retire(lib: &Library, memory: &SharedMemory, libraries: &Libraries) {
    lib.dead.store(true, Ordering::Relaxed);
    let data = crate::syscall::shared_bytes(memory);
    let _ = crate::memory::write_u64(data, lib.entry, 0);
    let _ = crate::memory::write_u64(data, lib.entry + 8, 0);
    if let Some(&epoch) = libraries.epoch.get()
        && let Ok(slice) = crate::memory::slice(data, epoch, 8)
    {
        // SAFETY: in bounds (checked) and 8-byte aligned (a u64 global).
        let counter = unsafe { std::sync::atomic::AtomicU64::from_ptr(slice.as_ptr() as *mut u64) };
        counter.fetch_add(1, Ordering::Release);
    }
}

/// Retires the translations of generated code in [start, end): the program
/// unmapped it or made it writable again (to change it).
pub fn retire_code(group: &crate::thread::Group, start: u64, end: u64) {
    let Some(threading) = group.threading.get() else { return };
    let libraries = &group.libraries;
    for lib in libraries.list.lock().unwrap().iter() {
        if lib.code && !lib.dead.load(Ordering::Relaxed) && lib.base < end && start < lib.end {
            retire(lib, &threading.memory, libraries);
        }
    }
}

/// Instantiates the libraries this store hasn't yet.
pub fn sync(mut store: impl AsContextMut<Data = Process>) -> Result<()> {
    let group = store.as_context().data().group.clone();
    let Some(threading) = group.threading.get() else { return Ok(()) };
    loop {
        let Some(main) = store.as_context().data().main.clone() else { return Ok(()) };
        let next = group.libraries.list.lock().unwrap().get(main.loaded).cloned();
        let Some(lib) = next else { return Ok(()) };
        instantiate(&mut store, &lib, &threading.memory)?;
        store.as_context_mut().data_mut().main.as_mut().unwrap().loaded += 1;
    }
}

/// One instance of a library in this store, linked against the store's
/// main instance, with its table slots filled.
fn instantiate(mut store: impl AsContextMut<Data = Process>, lib: &Library, memory: &SharedMemory) -> Result<Instance> {
    let main = store.as_context().data().main.clone().context("no main instance")?;
    let mut linker: Linker<Process> = Linker::new(store.as_context().engine());
    let mut got_globals = Vec::new();
    // GOT.func imports come in the order of `lib.got`: the k-th is slot k.
    let mut slots = lib.table_base..;
    for import in lib.module.imports() {
        let (module, name) = (import.module(), import.name());
        let ext: Extern = match (module, name) {
            ("env", "memory") => memory.clone().into(),
            ("env", "__indirect_function_table") => main.table.into(),
            ("env", "__memory_base") => {
                Global::new(&mut store, GlobalType::new(ValType::I64, Mutability::Const), Val::I64(lib.memory_base as i64))?.into()
            }
            ("env", "__table_base") => {
                Global::new(&mut store, GlobalType::new(ValType::I64, Mutability::Const), Val::I64(lib.table_base as i64))?.into()
            }
            ("env", other) => main
                .instance
                .get_export(&mut store, other)
                .with_context(|| format!("{}: the program doesn't provide `{other}`", lib.wasm.display()))?,
            ("GOT.func", sym) => {
                let slot = slots.next().unwrap();
                let g = Global::new(&mut store, GlobalType::new(ValType::I64, Mutability::Var), Val::I64(slot as i64))?;
                got_globals.push((sym.to_string(), slot));
                g.into()
            }
            _ => bail!("{}: unsupported import {module}.{name}", lib.wasm.display()),
        };
        linker.define(&store, module, name, ext)?;
    }
    let instance = linker.instantiate(&mut store, &lib.module)?;
    // Fill the table slots the GOT.func imports name.
    let needed = lib.table_base + lib.got.len() as u64;
    let size = main.table.size(&store);
    if needed > size {
        main.table.grow(&mut store, needed - size, Ref::Func(None))?;
    }
    for (sym, slot) in got_globals {
        let f = instance
            .get_func(&mut store, &sym)
            .with_context(|| format!("{}: `{sym}` isn't exported", lib.wasm.display()))?;
        main.table.set(&mut store, slot, Ref::Func(Some(f)))?;
    }
    Ok(instance)
}

/// Adds a library to the directory in guest memory that the support code's
/// `dispatch` searches (support/runtime.c, `struct module`), as entry `k`.
fn register(mut store: impl AsContextMut<Data = Process>, instance: &Instance, lib: &Library, memory: &SharedMemory, k: usize) -> Result<()> {
    let main = store.as_context().data().main.clone().context("no main instance")?;
    let data = crate::syscall::shared_bytes(memory);
    let word = |at: u64| -> Result<u64> { crate::memory::read_u64(data, at).map_err(|e| anyhow::anyhow!("errno {e}")) };
    // The library's exported data symbols are relative to its memory base.
    let addrs = lib.memory_base + global(&mut store, instance, "dispatch_addrs")?;
    let fns = lib.memory_base + global(&mut store, instance, "dispatch_fns")?;
    let count = word(lib.memory_base + global(&mut store, instance, "dispatch_count")?)?;
    let dir = global(&mut store, &main.instance, "__wasm_env_modules")?;
    let count_at = global(&mut store, &main.instance, "__wasm_env_module_count")?;
    const MODULES: usize = 1024;
    if k >= MODULES {
        bail!("too many libraries");
    }
    let entry = dir + 40 * k as u64;
    for (i, v) in [lib.base, lib.end, addrs, fns, count].into_iter().enumerate() {
        crate::memory::write_u64(data, entry + 8 * i as u64, v).map_err(|e| anyhow::anyhow!("errno {e}"))?;
    }
    // Publish the entry: the support code reads the count with acquire.
    let slice = crate::memory::slice(data, count_at, 8).map_err(|e| anyhow::anyhow!("errno {e}"))?;
    // SAFETY: in bounds (checked) and 8-byte aligned (a u64 global).
    let counter = unsafe { std::sync::atomic::AtomicU64::from_ptr(slice.as_ptr() as *mut u64) };
    counter.store(k as u64 + 1, std::sync::atomic::Ordering::Release);
    Ok(())
}

impl Libraries {
    /// A forked child's copy: the same libraries at the same places (its
    /// memory is a copy of the parent's).
    pub fn fork(&self) -> Libraries {
        Libraries {
            list: Mutex::new(self.list.lock().unwrap().clone()),
            next_slot: Mutex::new(*self.next_slot.lock().unwrap()),
            epoch: self.epoch.clone(),
        }
    }
}
