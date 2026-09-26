//! fork for translated programs (docs/abi.md, "Processes").
//!
//! A translated program's state at a syscall is all in its memory: the
//! guest's registers are in its thread's guest-state block, and the host
//! stack holds nothing the guest needs (the lifted code reached the syscall
//! through tail calls). So a child is a new process whose memory is a copy
//! of the parent's, running new instances of the program and its libraries
//! (with the same placements), started at the support code's
//! `__wasm_env_fork_start`, which resumes after the `clone` with x0 = 0.
//!
//! Programs built with the toolchain keep C state on the host stack, so
//! they can't be forked this way; for them fork stays ENOSYS (vfork works).

use wasmtime::{Caller, SharedMemory, Store, Val};

use crate::abi::errno::*;
use crate::memory::{self as mem, SysResult};
use crate::process::{self, Outcome, Process};

const PAGE: usize = 4096;

/// Copies the pages of [lo, hi) of `from` that hold something into `to`,
/// which starts out zero (or, for the guest image, as its file): untouched
/// pages read as zero, so a child costs only what the parent used.
fn copy_pages(from: &[u8], to: &mut [u8], lo: u64, hi: u64) {
    let (lo, hi) = (lo as usize / PAGE * PAGE, (hi as usize).next_multiple_of(PAGE).min(from.len()).min(to.len()));
    let mut at = lo;
    while at < hi {
        let end = (at + PAGE).min(hi);
        let (src, dst) = (&from[at..end], &mut to[at..end]);
        if src != dst {
            dst.copy_from_slice(src);
        }
        at = end;
    }
}

/// clone without CLONE_VM (and without CLONE_THREAD): fork. `frame` is the
/// support code's clone frame (where the child resumes). Returns the
/// child's pid.
pub fn fork(caller: &mut Caller<'_, Process>, args: &[i64; 6]) -> SysResult<i64> {
    const CLONE_PARENT_SETTID: i64 = 0x100000;
    const CLONE_CHILD_CLEARTID: i64 = 0x200000;
    const CLONE_CHILD_SETTID: i64 = 0x1000000;
    let [flags, _stack, ptid, _tls, ctid, frame] = *args;
    let p = caller.data();
    let (Some(threading), Some(main)) = (p.group.threading.get(), p.main.clone()) else {
        return Err(ENOSYS); // not a translated program
    };
    if !p.vfork.is_empty() {
        return Err(ENOSYS);
    }
    let (kernel, parent_memory, wasm) =
        (p.kernel.clone(), threading.memory.clone(), p.module.clone());
    // Where this thread's thread-local block and C stack are: the child's
    // instance uses the same ones (their contents are copied).
    let global = |caller: &mut Caller<'_, Process>, name: &str| match main.instance.get_global(&mut *caller, name).map(|g| g.get(&mut *caller)) {
        Some(Val::I64(v)) => Ok(v),
        _ => Err(ENOSYS),
    };
    let (tls_base, sp) = (global(caller, "__tls_base")?, global(caller, "__stack_pointer")?);

    // The child's memory: the guest image, then every page the parent uses.
    let memory = SharedMemory::new(caller.engine(), parent_memory.ty()).map_err(|_| ENOMEM)?;
    crate::image::map(&memory, &wasm).map_err(|_| ENOMEM)?;
    crate::syscall::grow_shared(&memory, parent_memory.data_size() as u64)?;
    let p = caller.data();
    let top = p.group.mm.lock().unwrap().space.top;
    let (from, to) = (crate::syscall::shared_bytes(&parent_memory), crate::syscall::shared_bytes(&memory));
    copy_pages(from, to, 0, top);
    for lib in p.group.libraries.list.lock().unwrap().iter() {
        copy_pages(from, to, lib.base, lib.end);
    }

    let pid = kernel.create(p.pid);
    if flags & CLONE_PARENT_SETTID != 0 {
        mem::write(from, ptid as u64, &pid.to_le_bytes())?;
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        mem::write(to, ctid as u64, &pid.to_le_bytes())?;
    }
    let child = Process {
        kernel: kernel.clone(),
        pid,
        tid: pid,
        memory: None,
        group: p.group.fork(memory),
        fds: p.fds.copy(),
        fs: p.fs.clone(),
        umask: p.umask,
        sigactions: p.sigactions.clone(),
        creds: p.creds.clone(),
        vfork: Vec::new(),
        pending: None,
        trace: p.trace,
        start: std::time::Instant::now(),
        module: wasm,
        clear_tid: if flags & CLONE_CHILD_CLEARTID != 0 { ctid as u64 } else { 0 },
        started: None,
        main: None,
    };
    kernel.spawn_from(pid, move || run_child(child, tls_base, sp, frame as u64));
    Ok(pid as i64)
}

/// Runs a forked child until it exits or execs.
fn run_child(process: Process, tls_base: i64, sp: i64, frame: u64) -> Outcome {
    let kernel = process.kernel.clone();
    let group = process.group.clone();
    let threading = group.threading.get().unwrap();
    let mut store = Store::new(&kernel.engine, process);
    process::set_deadline(&mut store);
    let result = (|| -> anyhow::Result<()> {
        // The memory is initialized already (the module's start function sees
        // so and does nothing).
        let instance = process::instantiate(&mut store, &threading.module, Some(&threading.memory))?;
        store.data_mut().main = crate::modules::Main::new(&mut store, instance);
        for (name, v) in [("__tls_base", tls_base), ("__stack_pointer", sp)] {
            let g = instance.get_global(&mut store, name).ok_or_else(|| anyhow::anyhow!("no {name} export"))?;
            g.set(&mut store, Val::I64(v))?;
        }
        crate::modules::sync(&mut store)?;
        let start = instance.get_typed_func::<i64, ()>(&mut store, "__wasm_env_fork_start")?;
        Ok(start.call(&mut store, frame as i64)?)
    })();
    let name = store.data().module.display().to_string();
    process::finish(&mut store, result, &name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_only_pages_that_differ() {
        let mut from = vec![0u8; 4 * PAGE];
        from[10] = 1;
        from[2 * PAGE + 5] = 2;
        let mut to = vec![0u8; 4 * PAGE];
        to[3 * PAGE] = 9; // outside the range: kept
        copy_pages(&from, &mut to, 0, 3 * PAGE as u64);
        assert_eq!((to[10], to[2 * PAGE + 5], to[3 * PAGE]), (1, 2, 9));
    }
}
