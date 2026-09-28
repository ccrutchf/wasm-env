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
    /// For code the program generated at run time (see translate_code): the
    /// addresses it was entered at (its translation's roots) and the bytes
    /// of [base, end) it was translated from. `path` just names it.
    pub code: Option<Code>,
    /// For a page of a program or library translated again at run time, the
    /// addresses it was made for (see patch); `path` just names it.
    pub patch: Option<Vec<u64>>,
    /// Its region table: (addresses, functions, count), in guest memory.
    regions: OnceLock<(u64, u64, u64)>,
    /// Its slot in the module directory (see register).
    slot: usize,
    /// Retired: its code was unmapped or changed (see retire).
    dead: std::sync::atomic::AtomicBool,
}

pub struct Code {
    roots: Vec<u64>,
    bytes: Vec<u8>,
}

/// The libraries a process has loaded, in load order.
#[derive(Default)]
pub struct Libraries {
    pub list: Mutex<Vec<Arc<Library>>>,
    /// The next free function-table slot (after the main module's).
    next_slot: Mutex<u64>,
    /// Directory slots: the number used so far, and those retired modules
    /// freed.
    slots: Mutex<(usize, Vec<usize>)>,
    /// Where the support code's directory globals are, once a module is
    /// loaded.
    globals: OnceLock<Globals>,
}

/// Addresses of the support code's globals for the module directory.
#[derive(Clone, Copy)]
struct Globals {
    modules: u64,
    count: u64,
    added: u64,
    epoch: u64,
}

impl Libraries {
    /// The library covering a guest address, if any (not a retired one).
    pub fn at(&self, addr: u64) -> Option<Arc<Library>> {
        self.list.lock().unwrap().iter().rev().find(|l| addr >= l.base && addr < l.end && !l.dead.load(Ordering::Relaxed)).cloned()
    }

    /// The shared library (not generated code, not a patch) covering `addr`.
    pub fn library_at(&self, addr: u64) -> Option<Arc<Library>> {
        let list = self.list.lock().unwrap();
        list.iter().find(|l| addr >= l.base && addr < l.end && l.code.is_none() && l.patch.is_none()).cloned()
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
    add(&mut store, &mut next_slot, path, wasm, base, end, None, None)
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
    code: Option<Code>,
    patch: Option<Vec<u64>>,
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
    if libraries.globals.get().is_none() {
        let globals = Globals {
            modules: global(&mut store, &main.instance, "__wasm_env_modules")?,
            count: global(&mut store, &main.instance, "__wasm_env_module_count")?,
            added: global(&mut store, &main.instance, "__wasm_env_modules_added")?,
            epoch: global(&mut store, &main.instance, "__wasm_env_code_epoch")?,
        };
        let _ = libraries.globals.set(globals);
    }
    // A directory slot: one a retired module freed, else a new one.
    let slot = {
        let mut slots = libraries.slots.lock().unwrap();
        match slots.1.pop() {
            Some(s) => s,
            None => {
                const MODULES: usize = 1024;
                if slots.0 >= MODULES {
                    bail!("too many modules");
                }
                slots.0 += 1;
                slots.0 - 1
            }
        }
    };
    let lib = Arc::new(Library {
        path,
        wasm,
        module,
        base,
        end,
        memory_base,
        table_base,
        got,
        code,
        patch,
        regions: OnceLock::new(),
        slot,
        dead: false.into(),
    });
    // The guest range: the dynamic linker maps a library there.
    crate::syscall::grow_shared(&threading.memory, end).map_err(|e| anyhow::anyhow!("errno {e}"))?;
    let instance = instantiate(&mut store, &lib, &threading.memory)?;
    // Relocate its data (function pointers become table slots) once.
    if let Some(f) = instance.get_func(&mut store, "__wasm_apply_data_relocs") {
        f.typed::<(), ()>(&store)?.call(&mut store, ())?;
    }
    libraries.list.lock().unwrap().push(lib.clone());
    if let Some(main) = store.as_context_mut().data_mut().main.as_mut() {
        main.loaded += 1;
    }
    register(&mut store, &instance, &lib, &threading.memory, libraries)?;
    Ok(lib)
}

/// A global's value (an i64) in an instance.
fn global(mut store: impl AsContextMut, inst: &Instance, name: &str) -> Result<u64> {
    match inst.get_global(&mut store, name).map(|g| g.get(&mut store)) {
        Some(Val::I64(v)) => Ok(v as u64),
        _ => bail!("missing export {name}"),
    }
}

/// A u64 global of the support code, in guest memory, as an atomic.
fn atomic(memory: &SharedMemory, at: u64) -> Option<&std::sync::atomic::AtomicU64> {
    let slice = crate::memory::slice(crate::syscall::shared_bytes(memory), at, 8).ok()?;
    // SAFETY: in bounds (checked) and 8-byte aligned (a u64 global); the
    // memory lives as long as the process.
    Some(unsafe { std::sync::atomic::AtomicU64::from_ptr(slice.as_ptr() as *mut u64) })
}

/// Code the program generated at run time (a JIT's), reached at `pc`: the
/// wasm-env private call behind the support code's `translate_code`.
/// Translates the executable mapping holding `pc`, as it is now, from
/// `pc` and the entries of the modules already on the pages that code
/// occupies, which the new module replaces: one module per page, as each
/// thread's dispatch cache maps a page to one region function. So a JIT
/// adding code costs a translation of just its pages. ENOEXEC if `pc`
/// isn't in such memory (a library's missed target, say) or was translated
/// as an entry already.
pub fn translate_code(mut store: impl AsContextMut<Data = Process>, pc: u64, object: u64, object_base: u64) -> crate::memory::SysResult<i64> {
    use crate::abi::errno::*;
    let process = store.as_context().data();
    let (kernel, group, trace) = (process.kernel.clone(), process.group.clone(), process.trace);
    let threading = group.threading.get().ok_or(ENOEXEC)?;
    let libraries = &group.libraries;
    let mut next_slot = libraries.next_slot.lock().unwrap();
    sync(&mut store).map_err(|_| EIO)?;
    // A block the translation of the program (the support code names the
    // object), or of a library, missed.
    let owner = if object != 0 {
        let name = crate::memory::cstr(crate::syscall::shared_bytes(&threading.memory), object)?.to_string();
        Some(Owner::Program(name, object_base))
    } else {
        libraries.library_at(pc).map(Owner::Library)
    };
    if let Some(owner) = owner {
        return patch(&mut store, &mut next_slot, pc, owner);
    }
    // Generated code: translated from pc already, but this thread's cache
    // named code since replaced.
    if let Some(lib) = libraries.at(pc)
        && let Some(code) = &lib.code
        && code.roots.contains(&pc)
    {
        return Ok(1);
    }
    let (lo, hi) = group.mm.lock().unwrap().code_at(pc).ok_or(ENOEXEC)?;
    const MAX_CODE: u64 = 256 << 20;
    if hi - lo > MAX_CODE {
        return Err(ENOEXEC);
    }
    let started = std::time::Instant::now();
    let bytes = crate::memory::slice(crate::syscall::shared_bytes(&threading.memory), lo, hi - lo)?.to_vec();
    let mut roots = std::collections::BTreeSet::from([pc]);
    let mut replaced: Vec<Arc<Library>> = Vec::new();
    let (wasm, start, end) = loop {
        let list: Vec<u64> = roots.iter().copied().filter(|r| (lo..hi).contains(r)).collect();
        let wasm = crate::translate::ensure_code(&bytes, lo, &list, &kernel.root, trace)?;
        let (start, end) = code_range(&wasm).ok_or(EIO)?;
        // Live modules on these pages: their entries join the translation.
        let more: Vec<Arc<Library>> = libraries
            .list
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.code.is_some() && !l.dead.load(Ordering::Relaxed) && l.base < end && start < l.end)
            .filter(|l| !replaced.iter().any(|r| Arc::ptr_eq(r, l)))
            .cloned()
            .collect();
        if more.is_empty() {
            break (wasm, start, end);
        }
        for l in more {
            roots.extend(&l.code.as_ref().unwrap().roots);
            replaced.push(l);
        }
    };
    let translated = started.elapsed();
    for old in &replaced {
        retire(old, &threading.memory, libraries);
    }
    let name = PathBuf::from(format!("[code {start:#x}]"));
    let code = Code { roots: roots.into_iter().collect(), bytes: bytes[(start - lo) as usize..(end.min(hi) - lo) as usize].to_vec() };
    add(&mut store, &mut next_slot, name, wasm, start, end, Some(code), None).map_err(|e| {
        eprintln!("wasm-env: loading code translated from {lo:#x}: {e:#}");
        EIO
    })?;
    if trace {
        eprintln!(
            "[wasm-env] translated code at {start:#x}..{end:#x} (entered at {pc:#x}, replacing {}): {:.3} s, loaded in {:.3} s",
            replaced.len(),
            translated.as_secs_f64(),
            (started.elapsed() - translated).as_secs_f64()
        );
    }
    Ok(0)
}

/// Whose translation missed a block: the program's own (the object's name
/// as its translation knows it, and base), or a library module's.
enum Owner {
    Program(String, u64),
    Library(Arc<Library>),
}

/// A block the translation of the program or a library missed, reached at
/// `pc` (an address computed at run time that discovery didn't find): the
/// target is recorded for the next translation, as before, and meanwhile
/// its page is translated again (translate::translate_page) and takes over
/// the page's entry in the owner's region table (or, if the translation
/// had no region there, is found through the module directory). So the run
/// goes on instead of stopping. ENOEXEC for a program translated together
/// with its dynamic linker, whose image isn't one object's.
fn patch(mut store: impl AsContextMut<Data = Process>, next_slot: &mut u64, pc: u64, owner: Owner) -> crate::memory::SysResult<i64> {
    use crate::abi::errno::*;
    let process = store.as_context().data();
    let (kernel, group, trace, program) = (process.kernel.clone(), process.group.clone(), process.trace, process.module.clone());
    let threading = group.threading.get().ok_or(ENOEXEC)?;
    let libraries = &group.libraries;
    let page = pc & !(crate::abi::PAGE_SIZE - 1);
    let current: Vec<Arc<Library>> = {
        let list = libraries.list.lock().unwrap();
        list.iter().filter(|l| l.base == page && l.patch.is_some() && !l.dead.load(Ordering::Relaxed)).cloned().collect()
    };
    // Patched for pc already: this thread's cache named the old code.
    if current.iter().any(|l| l.patch.as_ref().unwrap().contains(&pc)) {
        return Ok(1);
    }
    let started = std::time::Instant::now();
    let (elf, object, base, module, args) = match &owner {
        Owner::Program(name, base) => {
            let path = PathBuf::from(name);
            let elf = if path.exists() { path } else { kernel.root.join(name.trim_start_matches('/')) };
            let monolithic = crate::translate::monolithic();
            if monolithic && std::fs::read(&elf).is_ok_and(|b| crate::process::has_interpreter(&b)) {
                return Err(ENOEXEC);
            }
            let args: Vec<String> = if monolithic { Vec::new() } else { vec!["--object".into()] };
            (elf, name.clone(), *base, program, args)
        }
        Owner::Library(lib) => {
            let args = vec!["--object".into(), "--base".into(), format!("{:x}", lib.base)];
            (lib.path.clone(), lib.path.display().to_string(), lib.base, lib.wasm.clone(), args)
        }
    };
    crate::translate::remember_missed(&module, &object, pc - base);
    let wasm = crate::translate::translate_page(&elf, &module, &args, page, &kernel.root, trace)?;
    let translated = started.elapsed();
    let mut targets: Vec<u64> = current.iter().flat_map(|l| l.patch.clone().unwrap()).collect();
    targets.push(pc);
    let name = PathBuf::from(format!("[page {page:#x} of {object}]"));
    let lib = add(&mut store, next_slot, name, wasm, page, page + crate::abi::PAGE_SIZE, None, Some(targets)).map_err(|e| {
        eprintln!("wasm-env: loading page {page:#x} translated again: {e:#}");
        EIO
    })?;
    // The owner's region table: the program's (the main module exports it)
    // or the library's.
    let table = match &owner {
        Owner::Program(..) => {
            let main = store.as_context().data().main.clone().ok_or(EIO)?;
            let data = crate::syscall::shared_bytes(&threading.memory);
            let mut g = |name| global(&mut store, &main.instance, name).map_err(|_| EIO);
            let (addrs, fns, count_at) = (g("dispatch_addrs")?, g("dispatch_fns")?, g("dispatch_count")?);
            Some((addrs, fns, crate::memory::read_u64(data, count_at)?))
        }
        Owner::Library(l) => l.regions.get().copied(),
    };
    let data = crate::syscall::shared_bytes(&threading.memory);
    if let (Some((addrs, fns, count)), Some(&(_, new_fns, new_count))) = (table, lib.regions.get())
        && new_count > 0
    {
        // Binary search for the page's region, as dispatch does.
        let (mut lo, mut hi) = (0, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match crate::memory::read_u64(data, addrs + 8 * mid)?.cmp(&page) {
                std::cmp::Ordering::Equal => {
                    let f = crate::memory::read_u64(data, new_fns)?;
                    atomic(&threading.memory, fns + 8 * mid).ok_or(EIO)?.store(f, Ordering::Release);
                    break;
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
    }
    // Earlier patches of the page go; every thread's cache drops the page's
    // old function (retire bumps the epoch).
    for old in &current {
        retire(old, &threading.memory, libraries);
    }
    if current.is_empty()
        && let Some(epoch) = libraries.globals.get().and_then(|g| atomic(&threading.memory, g.epoch))
    {
        epoch.fetch_add(1, Ordering::Release);
    }
    if trace {
        eprintln!(
            "[wasm-env] translated page {page:#x} of {object} again (entered at {pc:#x}): {:.3} s, loaded in {:.3} s",
            translated.as_secs_f64(),
            (started.elapsed() - translated).as_secs_f64()
        );
    }
    Ok(0)
}

/// The guest range a module of generated code covers: its `wasm-env.library`
/// section (see the translator's `code_extent`), the last section.
fn code_range(wasm: &Path) -> Option<(u64, u64)> {
    let bytes = std::fs::read(wasm).ok()?;
    let name = b"wasm-env.library";
    let at = bytes.windows(name.len()).rposition(|w| w == name)? + name.len();
    let word = |k: usize| Some(u64::from_le_bytes(bytes.get(at + 8 * k..at + 8 * k + 8)?.try_into().ok()?));
    Some((word(0)?, word(1)?))
}

/// Retires a module: empties its directory entry (freeing the slot), so
/// dispatch no longer finds it, and bumps the code epoch, so every thread
/// clears its dispatch cache (which may name its regions) before it next
/// dispatches.
fn retire(lib: &Library, memory: &SharedMemory, libraries: &Libraries) {
    if lib.dead.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Some(g) = libraries.globals.get() {
        let entry = g.modules + 40 * lib.slot as u64;
        for at in [entry, entry + 8] {
            if let Some(word) = atomic(memory, at) {
                word.store(0, Ordering::Release);
            }
        }
        if let Some(epoch) = atomic(memory, g.epoch) {
            epoch.fetch_add(1, Ordering::Release);
        }
    }
    libraries.slots.lock().unwrap().1.push(lib.slot);
}

/// The live modules of generated code overlapping [start, end).
fn code_in(libraries: &Libraries, start: u64, end: u64) -> Vec<Arc<Library>> {
    let list = libraries.list.lock().unwrap();
    list.iter().filter(|l| l.code.is_some() && !l.dead.load(Ordering::Relaxed) && l.base < end && start < l.end).cloned().collect()
}

/// Retires the translations of generated code in [start, end): the program
/// unmapped it, or mapped something else there.
pub fn retire_code(group: &crate::thread::Group, start: u64, end: u64) {
    let Some(threading) = group.threading.get() else { return };
    for lib in code_in(&group.libraries, start, end) {
        retire(&lib, &threading.memory, &group.libraries);
    }
}

/// Retires the translations of generated code in [start, end) whose bytes
/// changed: the program made that memory executable again, after making it
/// writable to add code or change it. (A JIT adding a trace leaves the
/// others alone, and so do their translations.)
pub fn check_code(group: &crate::thread::Group, start: u64, end: u64) {
    let Some(threading) = group.threading.get() else { return };
    let data = crate::syscall::shared_bytes(&threading.memory);
    for lib in code_in(&group.libraries, start, end) {
        let code = lib.code.as_ref().unwrap();
        let now = crate::memory::slice(data, lib.base, code.bytes.len() as u64);
        if now.map_or(true, |now| now != code.bytes.as_slice()) {
            retire(&lib, &threading.memory, &group.libraries);
        }
    }
}

/// Instantiates the libraries this store hasn't yet (not retired ones: no
/// directory entry names them).
pub fn sync(mut store: impl AsContextMut<Data = Process>) -> Result<()> {
    let group = store.as_context().data().group.clone();
    let Some(threading) = group.threading.get() else { return Ok(()) };
    loop {
        let Some(main) = store.as_context().data().main.clone() else { return Ok(()) };
        let next = group.libraries.list.lock().unwrap().get(main.loaded).cloned();
        let Some(lib) = next else { return Ok(()) };
        if !lib.dead.load(Ordering::Relaxed) {
            instantiate(&mut store, &lib, &threading.memory)?;
        }
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
/// `dispatch` searches (support/runtime.c, `struct module`), in its slot,
/// and counts it as added (threads catch up on added modules before using
/// the directory's new entries).
fn register(mut store: impl AsContextMut<Data = Process>, instance: &Instance, lib: &Library, memory: &SharedMemory, libraries: &Libraries) -> Result<()> {
    let g = *libraries.globals.get().context("no module directory")?;
    let data = crate::syscall::shared_bytes(memory);
    let word = |at: u64| -> Result<u64> { crate::memory::read_u64(data, at).map_err(|e| anyhow::anyhow!("errno {e}")) };
    // The library's exported data symbols are relative to its memory base.
    let addrs = lib.memory_base + global(&mut store, instance, "dispatch_addrs")?;
    let fns = lib.memory_base + global(&mut store, instance, "dispatch_fns")?;
    let count = word(lib.memory_base + global(&mut store, instance, "dispatch_count")?)?;
    let _ = lib.regions.set((addrs, fns, count));
    let entry = g.modules + 40 * lib.slot as u64;
    // The range last, so a thread searching the directory meanwhile sees
    // the entry empty (a reused slot) or complete.
    let atomic = |at: u64| atomic(memory, at).context("module directory out of bounds");
    for (i, v) in [addrs, fns, count] .into_iter().enumerate() {
        atomic(entry + 16 + 8 * i as u64)?.store(v, Ordering::Release);
    }
    atomic(entry + 8)?.store(lib.end, Ordering::Release);
    atomic(entry)?.store(lib.base, Ordering::Release);
    atomic(g.added)?.fetch_add(1, Ordering::Release);
    atomic(g.count)?.fetch_max(lib.slot as u64 + 1, Ordering::Release);
    Ok(())
}

impl Libraries {
    /// A forked child's copy: the same libraries at the same places (its
    /// memory is a copy of the parent's).
    pub fn fork(&self) -> Libraries {
        let slots = self.slots.lock().unwrap();
        Libraries {
            list: Mutex::new(self.list.lock().unwrap().clone()),
            next_slot: Mutex::new(*self.next_slot.lock().unwrap()),
            slots: Mutex::new((slots.0, slots.1.clone())),
            globals: self.globals.clone(),
        }
    }
}
