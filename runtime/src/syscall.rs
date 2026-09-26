//! The `linux.syscall` import: Linux syscalls (generic/arm64 numbering) on
//! top of the host. See docs/abi.md.

use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasmtime::{Caller, SharedMemory};

use crate::abi::{self, AT_EMPTY_PATH, AT_FDCWD, AT_REMOVEDIR, AT_SYMLINK_NOFOLLOW, errno::*, nr, o};
use crate::fs::{self, DevPath, Device, Fd, Open};
use crate::kernel::Termination;
use crate::memory::{self as mem, SysResult, page_align};
use crate::process::{self, NSIG, Pending, Process, Signals, Unwind, VforkFrame};

/// Entry point for every syscall the program makes.
#[allow(clippy::too_many_arguments)]
pub fn syscall(
    mut caller: Caller<'_, Process>,
    n: i64,
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
) -> wasmtime::Result<i64> {
    let args = [a, b, c, d, e, f];
    deliver_signal(caller.data_mut(), false)?;
    let result = match n {
        nr::MMAP => mmap(&mut caller, &args),
        nr::MREMAP => mremap(&mut caller, &args),
        nr::BRK => brk(&mut caller, args[0] as u64),
        nr::WASM_ENV_LOAD_LIBRARY => load_library(&mut caller, a, b as u64),
        // fork: a copy of the process (fork.rs).
        nr::CLONE if a & (0x100 | CLONE_THREAD) == 0 => crate::fork::fork(&mut caller, &args),
        nr::WASM_ENV_SYNC_MODULES => crate::modules::sync(&mut caller).map(|()| 0).map_err(|e| {
            eprintln!("wasm-env: loading libraries: {e:#}");
            EIO
        }),
        _ => {
            let (data, process) = data_and_process(&mut caller);
            process.call(data, n, &args)
        }
    };
    let ret = result.unwrap_or_else(|e| -e);
    if caller.data().trace {
        let process = caller.data();
        let name = match abi::name(n) {
            "?" => format!("syscall {n}"),
            known => known.to_string(),
        };
        let tid = process.tid;
        let path = if n == nr::OPENAT {
            let (data, _) = data_and_process(&mut caller);
            mem::cstr(data, b as u64).map(|p| format!(" {p:?}")).unwrap_or_default()
        } else {
            String::new()
        };
        eprintln!("[wasm-env] pid {tid}: {name}({a:#x}, {b:#x}, {c:#x}) = {ret}{path}");
    }
    let process = caller.data_mut();
    if process.pending.is_some() {
        return Err(wasmtime::Error::new(Unwind));
    }
    deliver_signal(process, true)?;
    if process.signals.deliver.is_none()
        && let Some(old) = process.signals.suspended.take()
    {
        // sigsuspend ended without a handler to run: back to the old mask.
        process.signals.mask = old;
    }
    if let Some(sig) = caller.data_mut().signals.deliver.take() {
        // The support code sees it when this call returns.
        let _ = deliver_to_guest(&mut caller, sig);
    }
    Ok(ret)
}

/// Acts on the signals sent to this process that this thread doesn't
/// block: a default action that ends the process ends it; ignored ones are
/// dropped. At the end of a syscall (`at_exit`), a signal with a handler is
/// set to be delivered (see Signals::deliver and deliver_to_guest); at its
/// start, it's left for then. Also stops this thread if another has ended
/// the process.
fn deliver_signal(p: &mut Process, at_exit: bool) -> wasmtime::Result<()> {
    if p.group.ended() && p.vfork.is_empty() {
        p.pending = Some(Pending::GroupEnded);
        return Err(wasmtime::Error::new(Unwind));
    }
    while let Some(sig) = p.kernel.peek_signal(p.pid, p.signals.mask) {
        let handler = p.handler(sig as usize);
        let handled = handler > process::SIG_IGN && sig != 9;
        if handled && p.main.is_some() {
            // Handlers run in translated programs (the support code builds
            // the signal frame).
            if at_exit && p.signals.deliver.is_none() {
                p.kernel.take_signal(p.pid, p.signals.mask);
                p.signals.deliver = Some(sig);
            }
            return Ok(());
        }
        p.kernel.take_signal(p.pid, p.signals.mask);
        if let Some(how) = p.signal_effect(sig) {
            p.pending = Some(Pending::Terminate(how));
            return Err(wasmtime::Error::new(Unwind));
        }
    }
    Ok(())
}

/// Hands a signal to be delivered to the translated program's support code:
/// the handler's details go in this thread's guest-state block (support/
/// runtime.c, `struct signal_delivery`), which builds the signal frame on
/// the guest's stack and enters the handler when the syscall returns. The
/// thread then blocks what the handler's sa_mask says (and the signal
/// itself, without SA_NODEFER); SA_RESETHAND restores the default action.
fn deliver_to_guest(caller: &mut Caller<'_, Process>, sig: i32) -> SysResult<()> {
    const SA_NODEFER: u64 = 0x40000000;
    const SA_RESETHAND: u64 = 0x80000000;
    let main = caller.data().main.clone().ok_or(ENOSYS)?;
    let state = match main.instance.get_global(&mut *caller, "__wasm_env_state").map(|g| g.get(&mut *caller)) {
        Some(wasmtime::Val::I64(v)) => v as u64,
        _ => return Err(ENOSYS),
    };
    let (data, p) = data_and_process(caller);
    let action = p.sigactions.get(sig as usize);
    let word = |k: usize| u64::from_le_bytes(action[8 * k..8 * k + 8].try_into().unwrap());
    let (handler, flags, restorer, sa_mask) = (word(0), word(1), word(2), word(3));
    let (alt_sp, alt_flags, alt_size) = p.signals.altstack;
    // After sigsuspend, the handler returns to the mask from before it.
    let old_mask = p.signals.suspended.take().unwrap_or(p.signals.mask);
    let record = [sig as u64, handler, flags, restorer, old_mask, alt_sp, alt_flags as u64, alt_size];
    for (k, v) in record.iter().enumerate() {
        mem::write_u64(data, state + SIGNAL_DELIVERY + 8 * k as u64, *v)?;
    }
    p.signals.mask |= sa_mask | if flags & SA_NODEFER == 0 { 1 << (sig - 1) } else { 0 };
    p.signals.mask &= !((1 << 8) | (1 << 18)); // SIGKILL, SIGSTOP
    if flags & SA_RESETHAND != 0 {
        p.sigactions.set(sig as usize, [0; 32]);
    }
    Ok(())
}

/// Where `struct signal_delivery` is in `struct guest_state` (support/
/// runtime.c, which checks it).
const SIGNAL_DELIVERY: u64 = 66344;

/// A shared memory's bytes. Other threads of the process use them at the
/// same time, as the guest's own threads do: the runtime only touches what a
/// syscall's arguments point to, as a kernel would.
pub fn shared_bytes<'a>(memory: &SharedMemory) -> &'a mut [u8] {
    let data = memory.data();
    // SAFETY: the memory stays mapped while the process (which holds a
    // handle to it) runs, and never moves (Config::memory_may_move(false));
    // UnsafeCell<u8> has u8's layout.
    unsafe { std::slice::from_raw_parts_mut(data.as_ptr() as *mut u8, data.len()) }
}

/// The program's memory and the process state, for a syscall.
fn data_and_process<'a>(caller: &'a mut Caller<'_, Process>) -> (&'a mut [u8], &'a mut Process) {
    match caller.data().memory.clone().expect("memory is set before _start") {
        process::Mem::Local(m) => m.data_and_store_mut(caller),
        process::Mem::Shared(m) => (shared_bytes(&m), caller.data_mut()),
    }
}

/// The program's memory, from outside a syscall.
pub fn memory_bytes(store: &mut wasmtime::Store<Process>) -> &mut [u8] {
    match store.data().memory.clone().expect("memory is set") {
        process::Mem::Local(m) => m.data_mut(store),
        process::Mem::Shared(m) => shared_bytes(&m),
    }
}

/// wasm-env's `load_library(fd, range)`: the support code asks whether the
/// file open as `fd` is a library to load as a module of its own
/// (docs/abi.md, "Libraries"); if so, loads it (translating it the first
/// time) and writes its guest base and end at `range`. ENOEXEC if it isn't
/// an arm64 shared object (the file is mapped as data).
fn load_library(caller: &mut Caller<'_, Process>, fd: i64, range: u64) -> SysResult<i64> {
    let p = caller.data();
    if p.main.is_none() {
        return Err(ENOSYS); // the program's libraries are translated with it
    }
    let open = p.fds.get(fd)?.open.clone();
    let path = match &*open.lock().unwrap() {
        Open::File { path, .. } => p.fs.host(path),
        _ => return Err(ENOEXEC),
    };
    let mut head = [0u8; 20];
    let is_lib = std::fs::File::open(&path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| crate::translate::is_arm64_elf(&head) && u16::from_le_bytes([head[16], head[17]]) == 3);
    if !is_lib {
        return Err(ENOEXEC);
    }
    let lib = crate::modules::load(&mut *caller, &path).map_err(|e| {
        eprintln!("wasm-env: {}: {e:#}", path.display());
        ENOEXEC
    })?;
    let (data, _) = data_and_process(caller);
    mem::write_u64(data, range, lib.base)?;
    mem::write_u64(data, range + 8, lib.end)?;
    Ok(0)
}

const MAP_FIXED: i64 = 0x10;
const MAP_ANONYMOUS: i64 = 0x20;
const MREMAP_MAYMOVE: i64 = 1;

/// Grows linear memory so it covers `top` (the whole mmap region, or the
/// break).
fn grow_to(caller: &mut Caller<'_, Process>, top: u64) -> SysResult<()> {
    match caller.data().memory.clone().unwrap() {
        process::Mem::Local(m) => {
            let size = m.data_size(&*caller) as u64;
            if top > size {
                m.grow(&mut *caller, (top - size).div_ceil(65536)).map_err(|_| ENOMEM)?;
            }
        }
        process::Mem::Shared(m) => grow_shared(&m, top)?,
    }
    Ok(())
}

pub fn grow_shared(m: &SharedMemory, top: u64) -> SysResult<()> {
    // Another thread may grow it first: then this grows by less, or not at
    // all.
    loop {
        let size = m.data_size() as u64;
        if top <= size {
            return Ok(());
        }
        m.grow((top - size).div_ceil(65536)).map_err(|_| ENOMEM)?;
    }
}

/// Allocates `len` bytes of a shared memory's mmap region (a new thread's
/// thread-local block), zeroed.
pub fn alloc_shared(group: &crate::thread::Group, memory: &SharedMemory, len: u64) -> SysResult<u64> {
    let (addr, top) = {
        let mut mm = group.mm.lock().unwrap();
        (mm.space.alloc(len), mm.space.top)
    };
    grow_shared(memory, top)?;
    mem::zero(shared_bytes(memory), addr, len)?;
    Ok(addr)
}

fn mmap(caller: &mut Caller<'_, Process>, args: &[i64; 6]) -> SysResult<i64> {
    let [addr, len, _prot, flags, fd, off] = *args;
    if len <= 0 {
        return Err(EINVAL);
    }
    let len = page_align(len as u64);
    let (addr, top) = {
        let mut mm = caller.data().group.mm.lock().unwrap();
        let space = &mut mm.space;
        let addr = if flags & MAP_FIXED != 0 {
            let addr = addr as u64;
            if addr < space.start || !addr.is_multiple_of(abi::PAGE_SIZE) {
                return Err(EINVAL);
            }
            space.alloc_at(addr, len);
            addr
        } else {
            space.alloc(len)
        };
        (addr, space.top)
    };
    grow_to(caller, top)?;
    let (data, process) = data_and_process(caller);
    mem::zero(data, addr, len)?;
    let range = mem::slice_mut(data, addr, len)?;
    if flags & MAP_ANONYMOUS == 0 {
        // File mapping: copy the contents in (docs/abi.md; read-only for now).
        let open = process.fds.get(fd)?.open.clone();
        let mut open = open.lock().unwrap();
        let Open::File { file, .. } = &mut *open else { return Err(EACCES) };
        let pos = file.stream_position().map_err(|e| fs::errno(&e))?;
        file.seek(SeekFrom::Start(off as u64)).map_err(|e| fs::errno(&e))?;
        let mut filled = 0;
        while filled < range.len() {
            match file.read(&mut range[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(fs::errno(&e)),
            }
        }
        file.seek(SeekFrom::Start(pos)).map_err(|e| fs::errno(&e))?;
    }
    Ok(addr as i64)
}

/// brk: moves the break within [brk_start, brk_start + BRK_MAX), growing
/// linear memory to cover it and zeroing what it newly exposes. Like Linux,
/// it returns the new break, or the old one when the request can't be met.
fn brk(caller: &mut Caller<'_, Process>, want: u64) -> SysResult<i64> {
    let group = caller.data().group.clone();
    let mut mm = group.mm.lock().unwrap();
    let (old, start) = (mm.brk, mm.brk_start);
    if want < start || want > start + mem::BRK_MAX {
        return Ok(old as i64);
    }
    if grow_to(caller, want).is_err() {
        return Ok(old as i64);
    }
    if want > old {
        let (data, _) = data_and_process(caller);
        mem::zero(data, old, want - old)?;
    }
    mm.brk = want;
    Ok(want as i64)
}

fn mremap(caller: &mut Caller<'_, Process>, args: &[i64; 6]) -> SysResult<i64> {
    let [old, old_len, new_len, flags, ..] = *args;
    if flags & MREMAP_MAYMOVE == 0 || new_len <= 0 {
        return Err(ENOMEM);
    }
    let (old, old_len, new_len) = (old as u64, page_align(old_len as u64), page_align(new_len as u64));
    let (new, top) = {
        let mut mm = caller.data().group.mm.lock().unwrap();
        if !mm.space.contains(old, old_len) {
            return Err(EFAULT);
        }
        (mm.space.alloc(new_len), mm.space.top)
    };
    grow_to(caller, top)?;
    let (data, process) = data_and_process(caller);
    let keep = old_len.min(new_len) as usize;
    data.copy_within(old as usize..old as usize + keep, new as usize);
    mem::zero(data, new + keep as u64, new_len - keep as u64)?;
    process.group.mm.lock().unwrap().space.release(old, old_len);
    Ok(new as i64)
}

impl Process {
    /// Handles syscalls that only need access to memory, not the store.
    fn call(&mut self, data: &mut [u8], n: i64, args: &[i64; 6]) -> SysResult<i64> {
        let [a, b, c, d, e, g] = *args;
        let (ua, ub, uc) = (a as u64, b as u64, c as u64);
        match n {
            nr::READ => self.read(a, mem::slice_mut(data, ub, uc)?),
            nr::WRITE => self.write(a, mem::slice(data, ub, uc)?),
            nr::READV => {
                let mut total = 0;
                for (base, len) in mem::iovecs(data, ub, uc)? {
                    let n = self.read(a, mem::slice_mut(data, base, len)?)?;
                    total += n;
                    if (n as u64) < len {
                        break;
                    }
                }
                Ok(total)
            }
            nr::WRITEV => {
                // Gather first so one writev is one write (keeps lines intact).
                let mut buf = Vec::new();
                for (base, len) in mem::iovecs(data, ub, uc)? {
                    buf.extend_from_slice(mem::slice(data, base, len)?);
                }
                self.write(a, &buf)
            }
            nr::PREAD64 => self.pread(a, mem::slice_mut(data, ub, uc)?, d as u64),
            nr::PWRITE64 => self.pwrite(a, mem::slice(data, ub, uc)?, d as u64),
            nr::OPENAT => {
                let path = mem::cstr(data, ub)?;
                self.openat(a, &path, c)
            }
            nr::CLOSE => self.fds.close(a).map(|_| 0),
            nr::LSEEK => self.lseek(a, b, c),
            nr::GETDENTS64 => self.getdents64(a, mem::slice_mut(data, ub, uc)?),
            nr::NEWFSTATAT => {
                let path = mem::cstr(data, ub)?;
                let st = self.fstatat(a, &path, d)?;
                mem::write(data, uc, &st).map(|_| 0)
            }
            nr::STATX => {
                // statx(dirfd, path, flags, mask, buf): the basic fields, from stat.
                let path = mem::cstr(data, ub)?;
                let st = self.fstatat(a, &path, c)?;
                mem::write(data, e as u64, &fs::statx_from_stat(&st)).map(|_| 0)
            }
            nr::STATFS | nr::FSTATFS => mem::write(data, ub, &self.statfs()).map(|_| 0),
            // No extended attributes: nothing to list or get, and none can
            // be set.
            nr::GETXATTR | nr::LGETXATTR | nr::FGETXATTR => Err(ENODATA),
            nr::LISTXATTR | nr::LLISTXATTR | nr::FLISTXATTR => Ok(0),
            nr::SETXATTR | nr::LSETXATTR | nr::FSETXATTR | nr::REMOVEXATTR | nr::LREMOVEXATTR | nr::FREMOVEXATTR => {
                Err(ENOTSUP)
            }
            nr::MINCORE => {
                // mincore(addr, len, vec): pages of the program's memory (the
                // image, break and mmap region, and libraries) count as
                // resident; outside them is ENOMEM, as for unmapped memory
                // (programs probe for their stack's extent this way).
                let end = ua.checked_add(ub).ok_or(ENOMEM)?;
                let top = self.group.mm.lock().unwrap().space.top;
                let in_lib = self.group.libraries.list.lock().unwrap().iter().any(|l| ua >= l.base && end <= l.end);
                if end > top && !in_lib {
                    return Err(ENOMEM);
                }
                let pages = ub.div_ceil(abi::PAGE_SIZE);
                mem::slice_mut(data, uc, pages)?.fill(1);
                Ok(0)
            }
            nr::FSTAT => {
                let st = self.fstat(a)?;
                mem::write(data, ub, &st).map(|_| 0)
            }
            nr::READLINKAT => {
                let path = mem::cstr(data, ub)?;
                let dir = self.dir_of(a)?;
                let guest = self.fs.resolve(&dir, &path, false)?;
                let target = std::fs::read_link(self.fs.host(&guest)).map_err(|e| match fs::errno(&e) {
                    ENOENT => ENOENT,
                    _ => EINVAL,
                })?;
                let target = target.to_string_lossy().into_owned().into_bytes();
                let n = target.len().min(d as usize);
                mem::write(data, uc, &target[..n]).map(|_| n as i64)
            }
            nr::FACCESSAT => {
                let path = mem::cstr(data, ub)?;
                let dir = self.dir_of(a)?;
                if self.fs.device(&dir, &path).is_some() {
                    return Ok(0);
                }
                let guest = self.fs.resolve(&dir, &path, true)?;
                std::fs::metadata(self.fs.host(&guest)).map(|_| 0).map_err(|e| fs::errno(&e))
            }
            nr::GETCWD => {
                let mut cwd = self.fs.cwd().into_bytes();
                cwd.push(0);
                if (cwd.len() as u64) > ub {
                    return Err(ERANGE);
                }
                mem::write(data, ua, &cwd).map(|_| cwd.len() as i64)
            }
            nr::CHDIR => {
                let path = mem::cstr(data, ua)?;
                let guest = self.fs.resolve(&self.fs.cwd(), &path, true)?;
                let meta = std::fs::metadata(self.fs.host(&guest)).map_err(|e| fs::errno(&e))?;
                if !meta.is_dir() {
                    return Err(ENOTDIR);
                }
                self.fs.set_cwd(guest);
                Ok(0)
            }
            nr::FCHDIR => {
                let fd = self.fds.get(a)?;
                let Open::Dir { path, .. } = &*fd.open.lock().unwrap() else { return Err(ENOTDIR) };
                self.fs.set_cwd(path.clone());
                Ok(0)
            }
            nr::FCNTL if matches!(b, 5 | 6 | 7 | 36 | 37 | 38) => self.lock(data, a, b, uc),
            nr::FCNTL => self.fcntl(a, b, c),
            nr::FLOCK => self.flock(a, b),
            // Sockets (net.rs).
            nr::SOCKET => self.socket(a, b, c),
            nr::CONNECT => self.connect(data, a, ub, uc),
            nr::BIND => self.bind(data, a, ub, uc),
            nr::LISTEN => self.listen(a, b),
            nr::ACCEPT => self.accept(data, a, ub, uc, 0),
            nr::ACCEPT4 => self.accept(data, a, ub, uc, d),
            nr::GETSOCKNAME => self.sock_name(data, a, ub, uc, false),
            nr::GETPEERNAME => self.sock_name(data, a, ub, uc, true),
            nr::SENDTO => self.sendto(data, a, ub, uc, d, e as u64, g as u64),
            nr::RECVFROM => self.recvfrom(data, a, ub, uc, d, e as u64, g as u64),
            nr::SENDMSG => self.sendmsg(data, a, ub, c),
            nr::RECVMSG => self.recvmsg(data, a, ub, c),
            nr::SENDMMSG => self.sendmmsg(data, a, ub, uc, d),
            nr::SHUTDOWN => self.shutdown(a, b),
            nr::SETSOCKOPT => self.setsockopt(data, a, b, c, d as u64, e as u64),
            nr::GETSOCKOPT => self.getsockopt(data, a, b, c, d as u64, e as u64),
            nr::PPOLL => self.ppoll(data, ua, ub, uc),
            nr::PSELECT6 => self.pselect(data, a, [ub, uc, d as u64], e as u64),
            nr::IOCTL => self.ioctl(data, a, b, uc),
            nr::DUP => {
                let fd = Fd { cloexec: false, ..self.fds.get(a)?.clone() };
                self.fds.insert(fd, 0)
            }
            nr::DUP3 => {
                if a == b {
                    return Err(EINVAL);
                }
                let fd = Fd { cloexec: c & o::CLOEXEC != 0, ..self.fds.get(a)?.clone() };
                self.fds.set(usize::try_from(b).map_err(|_| EBADF)?, fd);
                Ok(b)
            }
            nr::MKDIRAT => {
                let path = mem::cstr(data, ub)?;
                let dir = self.dir_of(a)?;
                let guest = self.fs.resolve(&dir, &path, false)?;
                std::fs::create_dir(self.fs.host(&guest)).map(|_| 0).map_err(|e| fs::errno(&e))
            }
            nr::UNLINKAT => {
                let path = mem::cstr(data, ub)?;
                let dir = self.dir_of(a)?;
                let host = self.fs.host(&self.fs.resolve(&dir, &path, false)?);
                let r = if c & AT_REMOVEDIR != 0 { std::fs::remove_dir(host) } else { std::fs::remove_file(host) };
                r.map(|_| 0).map_err(|e| fs::errno(&e))
            }
            nr::RENAMEAT | nr::RENAMEAT2 => {
                // renameat2 flags (RENAME_NOREPLACE etc.) aren't supported yet.
                if n == nr::RENAMEAT2 && args[4] != 0 {
                    return Err(EINVAL);
                }
                let from = self.at_path(data, a, ub, false)?;
                let to = self.at_path(data, c, d as u64, false)?;
                std::fs::rename(from, to).map(|_| 0).map_err(|e| fs::errno(&e))
            }
            nr::LINKAT => {
                let from = self.at_path(data, a, ub, args[4] & 0x400 != 0)?; // AT_SYMLINK_FOLLOW
                let to = self.at_path(data, c, d as u64, false)?;
                std::fs::hard_link(from, to).map(|_| 0).map_err(|e| fs::errno(&e))
            }
            nr::SYMLINKAT => {
                // The target is stored as written; it's resolved in guest
                // terms whenever the link is followed (fs::Fs::resolve).
                let target = mem::cstr(data, ua)?;
                let link = self.at_path(data, b, uc, false)?;
                symlink(&target, &link).map(|_| 0).map_err(|e| fs::errno(&e))
            }
            nr::FCHMODAT => {
                let path = self.at_path(data, a, ub, true)?;
                chmod(&path, c as u32).map(|_| 0).map_err(|e| fs::errno(&e))
            }
            nr::FCHMOD => {
                let open = self.fds.get(a)?.open.clone();
                let path = match &*open.lock().unwrap() {
                    Open::File { path, .. } | Open::Dir { path, .. } => self.fs.host(path),
                    _ => return Err(EINVAL),
                };
                chmod(&path, b as u32).map(|_| 0).map_err(|e| fs::errno(&e))
            }
            // Everything in the environment is owned by root; ownership
            // changes are accepted and ignored.
            nr::FCHOWNAT | nr::FCHOWN => Ok(0),
            nr::FTRUNCATE => self.with_file(a, |f| f.set_len(ub)).map(|_| 0),
            nr::FSYNC | nr::FDATASYNC => {
                // Syncing a directory (dpkg does, after renaming into it): its
                // entries are the host's, already written through.
                if matches!(&*self.fds.get(a)?.open.lock().unwrap(), Open::Dir { .. }) {
                    return Ok(0);
                }
                self.with_file(a, |f| f.sync_all()).map(|_| 0)
            }
            nr::UTIMENSAT => {
                // times: [atime, mtime] timespecs, or NULL for "now".
                const UTIME_NOW: i64 = (1 << 30) - 1;
                const UTIME_OMIT: i64 = (1 << 30) - 2;
                let now = SystemTime::now();
                let mut set = std::fs::FileTimes::new();
                for (i, is_mtime) in [(0, false), (1, true)] {
                    let t = if c == 0 {
                        Some(now)
                    } else {
                        let ts = mem::slice(data, uc + i * 16, 16)?;
                        let secs = i64::from_le_bytes(ts[..8].try_into().unwrap());
                        let nanos = i64::from_le_bytes(ts[8..].try_into().unwrap());
                        match nanos {
                            UTIME_OMIT => None,
                            UTIME_NOW => Some(now),
                            _ => Some(UNIX_EPOCH + Duration::new(secs.max(0) as u64, nanos as u32)),
                        }
                    };
                    if let Some(t) = t {
                        set = if is_mtime { set.set_modified(t) } else { set.set_accessed(t) };
                    }
                }
                let apply = |f: &std::fs::File| f.set_times(set).map(|_| 0).map_err(|e| fs::errno(&e));
                if b == 0 {
                    // futimens(fd, times)
                    return self.with_file(a, |f| Ok(apply(f))).and_then(|r| r);
                }
                let path = mem::cstr(data, ub)?;
                let dir = self.dir_of(a)?;
                let guest = self.fs.resolve(&dir, &path, d & AT_SYMLINK_NOFOLLOW == 0)?;
                let host = self.fs.host(&guest);
                if d & AT_SYMLINK_NOFOLLOW != 0 && host.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                    // A symlink's own times (dpkg sets them on links it unpacks,
                    // whose targets may not exist yet): not kept.
                    return Ok(0);
                }
                let file = std::fs::File::open(host).map_err(|e| fs::errno(&e))?;
                apply(&file)
            }
            nr::SET_TID_ADDRESS => {
                self.clear_tid = ua;
                Ok(self.tid as i64)
            }
            nr::GETPID => Ok(self.pid as i64),
            // In a vfork child (a process of its own), the thread is the process.
            nr::GETTID => Ok(if self.vfork.is_empty() { self.tid } else { self.pid } as i64),
            nr::GETPPID => Ok(self.kernel.ppid(self.pid) as i64),
            // User and group ids (see process::Creds): [real, effective, saved].
            nr::GETUID => Ok(self.creds.uid[0] as i64),
            nr::GETEUID => Ok(self.creds.uid[1] as i64),
            nr::GETGID => Ok(self.creds.gid[0] as i64),
            nr::GETEGID => Ok(self.creds.gid[1] as i64),
            nr::SETUID => Ok(set_ids(&mut self.creds.uid, [a, a, a])),
            nr::SETGID => Ok(set_ids(&mut self.creds.gid, [a, a, a])),
            nr::SETREUID => Ok(set_ids(&mut self.creds.uid, [a, b, -1])),
            nr::SETREGID => Ok(set_ids(&mut self.creds.gid, [a, b, -1])),
            nr::SETRESUID => Ok(set_ids(&mut self.creds.uid, [a, b, c])),
            nr::SETRESGID => Ok(set_ids(&mut self.creds.gid, [a, b, c])),
            nr::GETRESUID | nr::GETRESGID => {
                let ids = if n == nr::GETRESUID { self.creds.uid } else { self.creds.gid };
                for (at, id) in [ua, ub, uc].into_iter().zip(ids) {
                    mem::write(data, at, &id.to_le_bytes())?;
                }
                Ok(0)
            }
            // The filesystem ids follow the effective ones; report those.
            nr::SETFSUID => Ok(self.creds.uid[1] as i64),
            nr::SETFSGID => Ok(self.creds.gid[1] as i64),
            nr::GETGROUPS => {
                let groups = &self.creds.groups;
                if a != 0 {
                    if (a as usize) < groups.len() {
                        return Err(EINVAL);
                    }
                    let bytes: Vec<u8> = groups.iter().flat_map(|g| g.to_le_bytes()).collect();
                    mem::write(data, ub, &bytes)?;
                }
                Ok(groups.len() as i64)
            }
            nr::SETGROUPS => {
                let n = usize::try_from(a).ok().filter(|&n| n <= 65536).ok_or(EINVAL)?;
                let bytes = mem::slice(data, ub, n as u64 * 4)?;
                self.creds.groups = bytes.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
                Ok(0)
            }
            nr::EXIT if self.vfork.is_empty() && self.group.threads() > 1 => {
                self.pending = Some(Pending::ThreadExit(a as i32 & 0xff));
                Ok(0)
            }
            nr::EXIT | nr::EXIT_GROUP => Ok(self.exit(a as i32 & 0xff)),
            nr::WASM_ENV_MISSED_TARGET if ub == 0 => {
                // In a library loaded as its own module: record it there.
                if let Some(lib) = self.group.libraries.at(ua) {
                    crate::translate::record_missed_target(&lib.wasm, &lib.path.display().to_string(), ua - lib.base);
                }
                Ok(0)
            }
            nr::WASM_ENV_MISSED_TARGET => {
                crate::translate::record_missed_target(&self.module, &mem::cstr(data, ub)?, uc);
                Ok(0)
            }
            nr::KILL => self.kernel.kill(self.pid, a as i32, b as i32).map(|_| 0),
            nr::TGKILL => self.kernel.kill(self.pid, a as i32, c as i32).map(|_| 0),
            // tkill(tid, sig): signals are per process here; a thread of this
            // one (raise() in musl) means this process, else the process with
            // that id.
            nr::TKILL => {
                let target = if a as i32 == self.tid || a as i32 == self.pid { self.pid } else { a as i32 };
                self.kernel.kill(self.pid, target, b as i32).map(|_| 0)
            }
            nr::CLONE if a & CLONE_THREAD != 0 => self.clone_thread(data, a, b, c, d, e, g),
            nr::CLONE => self.clone_vfork(a, b),
            nr::WASM_ENV_THREAD_STARTED => {
                if let Some(started) = self.started.take() {
                    let _ = started.send(());
                }
                Ok(0)
            }
            nr::EXECVE => {
                let path = mem::cstr(data, ua)?;
                let argv = string_array(data, ub)?;
                let env = string_array(data, uc)?;
                let image = process::resolve_exec(&self.fs, &path, argv, env, self.trace)?;
                if self.vfork.is_empty() {
                    // Unwind out of this program; the process loop starts the next.
                    self.pending = Some(Pending::Exec(image));
                    return Ok(0);
                }
                // A vfork child: start the new program as the child and hand
                // the parent back its state. libc then jumps back to vfork.
                let inherit = self.inherit();
                let child = self.vfork_return();
                self.kernel.spawn(child, image, inherit);
                Ok(child as i64)
            }
            nr::WAIT4 => {
                const WNOHANG: i64 = 1;
                match self.kernel.wait(self.pid, a as i32, c & WNOHANG != 0, self.interrupting())? {
                    None => Ok(0),
                    Some((child, how)) => {
                        if b != 0 {
                            mem::write(data, ub, &how.wait_status().to_le_bytes())?;
                        }
                        if d != 0 {
                            mem::slice_mut(data, d as u64, 144)?.fill(0); // struct rusage
                        }
                        Ok(child as i64)
                    }
                }
            }
            nr::PIPE2 => {
                let (r, w) = crate::pipe::new();
                // Each end has its access mode (fdopen checks it with F_GETFL).
                let fd = |end: crate::pipe::PipeEnd| Fd {
                    cloexec: b & o::CLOEXEC != 0,
                    flags: (b & o::NONBLOCK) | if end.write { o::WRONLY } else { 0 },
                    open: Arc::new(Mutex::new(Open::Pipe(end))),
                };
                let r = self.fds.insert(fd(r), 0)?;
                let w = self.fds.insert(fd(w), 0)?;
                let mut pair = [0u8; 8];
                pair[..4].copy_from_slice(&(r as i32).to_le_bytes());
                pair[4..].copy_from_slice(&(w as i32).to_le_bytes());
                mem::write(data, ua, &pair).map(|_| 0)
            }
            nr::SETPGID => {
                let pid = if a == 0 { self.pid } else { a as i32 };
                let pgid = if b == 0 { pid } else { b as i32 };
                self.kernel.setpgid(pid, pgid).map(|_| 0)
            }
            nr::GETPGID => self.kernel.pgid(if a == 0 { self.pid } else { a as i32 }).map(i64::from),
            nr::SETSID => self.kernel.setsid(self.pid).map(i64::from),
            nr::GETSID => self.kernel.sid(if a == 0 { self.pid } else { a as i32 }).map(i64::from),
            nr::SET_ROBUST_LIST | nr::MPROTECT | nr::MADVISE => Ok(0),
            nr::RT_SIGACTION => {
                // Handlers are recorded and reported back, but not yet run;
                // SIG_IGN takes effect (see Process::signal_effect).
                let sig = usize::try_from(a).ok().filter(|s| (1..NSIG).contains(s)).ok_or(EINVAL)?;
                if c != 0 {
                    mem::write(data, uc, &self.sigactions.get(sig))?;
                }
                if b != 0 {
                    if sig == 9 || sig == 19 {
                        return Err(EINVAL); // SIGKILL and SIGSTOP can't be changed
                    }
                    self.sigactions.set(sig, mem::slice(data, ub, 32)?.try_into().unwrap());
                }
                Ok(0)
            }
            nr::RT_SIGPROCMASK => {
                // rt_sigprocmask(how, set, oldset, size): SIGKILL and SIGSTOP
                // can't be blocked.
                const SIG_BLOCK: i64 = 0;
                const SIG_UNBLOCK: i64 = 1;
                const SIG_SETMASK: i64 = 2;
                if c != 0 {
                    mem::write_u64(data, uc, self.signals.mask)?;
                }
                if b != 0 {
                    let set = mem::read_u64(data, ub)?;
                    self.signals.mask = match a {
                        SIG_BLOCK => self.signals.mask | set,
                        SIG_UNBLOCK => self.signals.mask & !set,
                        SIG_SETMASK => set,
                        _ => return Err(EINVAL),
                    } & !((1 << 8) | (1 << 18));
                }
                Ok(0)
            }
            nr::SETITIMER | nr::GETITIMER => {
                // struct itimerval { interval, value }, each { sec, usec }.
                // Only ITIMER_REAL (SIGALRM, in real time).
                if a != 0 {
                    return Err(EINVAL);
                }
                let tv = |d: Duration| [d.as_secs(), d.subsec_micros() as u64].map(u64::to_le_bytes).concat();
                let read_tv = |data: &[u8], at: u64| -> SysResult<Duration> {
                    let (s, us) = (mem::read_u64(data, at)?, mem::read_u64(data, at + 8)?);
                    Ok(Duration::from_secs(s) + Duration::from_micros(us))
                };
                let (left, interval) = if n == nr::GETITIMER {
                    self.kernel.timer(self.pid)
                } else if ub != 0 {
                    let (interval, value) = (read_tv(data, ub)?, read_tv(data, ub + 16)?);
                    self.kernel.set_timer(self.pid, value, interval)
                } else {
                    self.kernel.timer(self.pid)
                };
                let out = if n == nr::GETITIMER { ub } else { uc };
                if out != 0 {
                    mem::write(data, out, &[tv(interval), tv(left)].concat())?;
                }
                Ok(0)
            }
            // Programs built with the toolchain can't run handlers, which is what
            // sigsuspend waits for: they fall back to other ways of waiting.
            nr::RT_SIGSUSPEND if self.main.is_none() => Err(ENOSYS),
            nr::RT_SIGSUSPEND => {
                // Wait, with the given mask, for a signal that does something.
                // The old mask is restored once it's delivered (the handler's
                // frame saves it; see deliver_to_guest).
                let new = mem::read_u64(data, ua)? & !((1 << 8) | (1 << 18));
                self.signals.suspended = Some(std::mem::replace(&mut self.signals.mask, new));
                while !self.interrupted()() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(EINTR)
            }
            nr::RT_SIGPENDING => {
                let pending = self.kernel.pending(self.pid) & self.signals.mask;
                mem::write_u64(data, ua, pending).map(|_| 0)
            }
            nr::SIGALTSTACK => {
                // sigaltstack(ss, old): stack_t { ss_sp, ss_flags (int), ss_size }.
                if ub != 0 {
                    let (sp, flags, size) = self.signals.altstack;
                    mem::write_u64(data, ub, sp)?;
                    mem::write(data, ub + 8, &(flags as i32).to_le_bytes())?;
                    mem::write_u64(data, ub + 16, size)?;
                }
                if ua != 0 {
                    let sp = mem::read_u64(data, ua)?;
                    let flags = u32::from_le_bytes(mem::slice(data, ua + 8, 4)?.try_into().unwrap());
                    let size = mem::read_u64(data, ua + 16)?;
                    self.signals.altstack = (sp, flags, size);
                }
                Ok(0)
            }
            nr::UNAME => {
                let mut buf = [0u8; 65 * 6];
                for (i, s) in ["Linux", "wasm-env", "6.1.0-wasm-env", "#1", "wasm64", "(none)"].iter().enumerate() {
                    buf[i * 65..i * 65 + s.len()].copy_from_slice(s.as_bytes());
                }
                mem::write(data, ua, &buf).map(|_| 0)
            }
            nr::CLOCK_GETTIME => {
                let t = self.clock(a)?;
                mem::write(data, ub, &timespec(t)).map(|_| 0)
            }
            nr::GETTIMEOFDAY => {
                if a != 0 {
                    let t = self.clock(0)?;
                    let mut tv = [0u8; 16];
                    tv[..8].copy_from_slice(&(t.as_secs() as i64).to_le_bytes());
                    tv[8..].copy_from_slice(&(t.subsec_micros() as i64).to_le_bytes());
                    mem::write(data, ua, &tv)?;
                }
                Ok(0)
            }
            nr::NANOSLEEP => self.sleep(data, ua, ub),
            nr::FUTEX => self.futex(data, args),
            nr::CLOCK_NANOSLEEP => {
                const TIMER_ABSTIME: i64 = 1;
                if b & TIMER_ABSTIME != 0 {
                    // Sleep until an absolute time on clock `a`.
                    let until = read_timespec(data, uc)?;
                    let now = self.clock(a)?;
                    let left = until.saturating_sub(now);
                    return self.sleep_for(left).map(|_| 0).map_err(|(e, _)| e);
                }
                self.sleep(data, uc, d as u64)
            }
            nr::SCHED_GETAFFINITY => {
                // Every host CPU (threads really run in parallel).
                let cpus = cpus() as u64;
                let mask = mem::slice_mut(data, uc, ub.min(128))?;
                mask.fill(0);
                for cpu in 0..cpus.min(mask.len() as u64 * 8) {
                    mask[cpu as usize / 8] |= 1 << (cpu % 8);
                }
                Ok(mask.len().min(cpus.div_ceil(64) as usize * 8) as i64)
            }
            nr::UMASK => Ok(std::mem::replace(&mut self.umask, (a & 0o777) as u32) as i64),
            nr::MUNMAP => {
                let len = page_align(ub);
                let mut mm = self.group.mm.lock().unwrap();
                if mm.space.contains(ua, len) {
                    mm.space.release(ua, len);
                }
                Ok(0)
            }
            nr::PRLIMIT64 => {
                // prlimit64(pid, resource, new, old): the current limits go to
                // `old`; new limits are accepted and ignored.
                if d != 0 {
                    const RLIMIT_STACK: i64 = 3;
                    const RLIMIT_NOFILE: i64 = 7;
                    let (cur, max) = match b {
                        RLIMIT_STACK => (8 << 20, 8 << 20),
                        RLIMIT_NOFILE => (1024, 1024),
                        _ => (u64::MAX, u64::MAX),
                    };
                    mem::write_u64(data, d as u64, cur)?;
                    mem::write_u64(data, d as u64 + 8, max)?;
                }
                Ok(0)
            }
            nr::GETRANDOM => {
                getrandom::fill(mem::slice_mut(data, ua, ub)?).map_err(|_| EIO)?;
                Ok(b)
            }
            _ => Err(ENOSYS),
        }
    }

    /// The host path for an *at() call's (dirfd, path) pair.
    fn at_path(&self, data: &[u8], dirfd: i64, path: u64, follow: bool) -> SysResult<std::path::PathBuf> {
        let path = mem::cstr(data, path)?;
        let dir = self.dir_of(dirfd)?;
        Ok(self.fs.host(&self.fs.resolve(&dir, &path, follow)?))
    }

    /// The guest directory an *at() call is relative to.
    fn dir_of(&self, dirfd: i64) -> SysResult<String> {
        // A descriptor is an int: AT_FDCWD may arrive without sign extension.
        let dirfd = dirfd as i32 as i64;
        if dirfd == AT_FDCWD {
            return Ok(self.fs.cwd());
        }
        let fd = self.fds.get(dirfd)?;
        match &*fd.open.lock().unwrap() {
            Open::Dir { path, .. } => Ok(path.clone()),
            _ => Err(ENOTDIR),
        }
    }

    /// exit/exit_group. In a vfork child this records the child's exit and
    /// returns its pid to libc, which jumps back to the parent.
    fn exit(&mut self, code: i32) -> i64 {
        if self.vfork.is_empty() {
            self.pending = Some(Pending::Terminate(Termination::Exit(code)));
            return 0;
        }
        let child = self.pid;
        self.kernel.exit(child, Termination::Exit(code));
        self.vfork_return();
        child as i64
    }

    /// clone. Only vfork (CLONE_VM | CLONE_VFORK, no new stack) is
    /// supported: the child gets its own pid and a copy of the process
    /// state, and keeps running in the parent's memory until it execs or
    /// exits (docs/abi.md, "Processes").
    fn clone_vfork(&mut self, flags: i64, stack: i64) -> SysResult<i64> {
        const CLONE_VM: i64 = 0x100;
        const CLONE_VFORK: i64 = 0x4000;
        if flags & (CLONE_VM | CLONE_VFORK) != CLONE_VM | CLONE_VFORK || stack != 0 {
            return Err(ENOSYS);
        }
        let child = self.kernel.create(self.pid);
        // The child gets its own descriptor table (the parent's may be shared
        // with its other threads).
        let own_fds = self.fds.copy();
        let own_sigactions = self.sigactions.copy();
        let own_fs = self.fs.copy();
        self.vfork.push(VforkFrame {
            parent_pid: self.pid,
            fds: std::mem::replace(&mut self.fds, own_fds),
            fs: std::mem::replace(&mut self.fs, own_fs),
            umask: self.umask,
            sigactions: std::mem::replace(&mut self.sigactions, own_sigactions),
            creds: self.creds.clone(),
            sigmask: self.signals.mask,
        });
        self.pid = child;
        Ok(0)
    }

    /// Ends the current vfork child: restores the parent's state and
    /// returns the child's pid.
    fn vfork_return(&mut self) -> i32 {
        let frame = self.vfork.pop().expect("in a vfork child");
        let child = std::mem::replace(&mut self.pid, frame.parent_pid);
        self.fds = frame.fds;
        self.fs = frame.fs;
        self.umask = frame.umask;
        self.sigactions = frame.sigactions;
        self.creds = frame.creds;
        self.signals.mask = frame.sigmask;
        child
    }

    /// nanosleep(req, rem): sleeps, but wakes early (EINTR, with the time
    /// left in `rem`) if a signal arrives.
    fn sleep(&self, data: &mut [u8], req: u64, rem: u64) -> SysResult<i64> {
        let duration = read_timespec(data, req)?;
        match self.sleep_for(duration) {
            Ok(()) => Ok(0),
            Err((e, left)) => {
                if rem != 0 {
                    mem::write(data, rem, &timespec(left))?;
                }
                Err(e)
            }
        }
    }

    /// futex, across the process's threads (thread::Futexes). Futexes are
    /// per process: processes don't share memory.
    fn futex(&self, data: &mut [u8], args: &[i64; 6]) -> SysResult<i64> {
        const WAIT: i64 = 0;
        const WAKE: i64 = 1;
        const REQUEUE: i64 = 3;
        const CMP_REQUEUE: i64 = 4;
        const WAIT_BITSET: i64 = 9;
        const WAKE_BITSET: i64 = 10;
        const CLOCK_REALTIME_FLAG: i64 = 256;
        let [addr, op, val, timeout, addr2, val3] = *args;
        let (addr, val) = (addr as u64, val as u32);
        let futexes = &self.group.futex;
        match op & 0x7f {
            WAIT | WAIT_BITSET => {
                let bitset = if op & 0x7f == WAIT { u32::MAX } else { val3 as u32 };
                // FUTEX_WAIT's timeout is relative; FUTEX_WAIT_BITSET's is a
                // deadline on CLOCK_MONOTONIC, or CLOCK_REALTIME if flagged.
                let deadline = if timeout == 0 {
                    None
                } else {
                    let t = read_timespec(data, timeout as u64)?;
                    let wait = if op & 0x7f == WAIT {
                        t
                    } else {
                        let now = self.clock(if op & CLOCK_REALTIME_FLAG != 0 { 0 } else { 1 })?;
                        t.saturating_sub(now)
                    };
                    Some(std::time::Instant::now() + wait)
                };
                futexes.wait(data, addr, val, bitset, deadline, &self.interrupted())
            }
            WAKE => Ok(futexes.wake(addr, val, u32::MAX)),
            WAKE_BITSET => Ok(futexes.wake(addr, val, val3 as u32)),
            // For requeues the timeout argument is the number to requeue.
            REQUEUE => futexes.requeue(data, addr, val, timeout as u32, addr2 as u64, None),
            CMP_REQUEUE => futexes.requeue(data, addr, val, timeout as u32, addr2 as u64, Some(val3 as u32)),
            _ => Err(ENOSYS),
        }
    }

    /// clone(CLONE_THREAD): a new thread in this process (docs/abi.md,
    /// "Threads"). The support code passes the new thread's registers in
    /// `frame`; the thread starts there once running (thread::spawn).
    #[allow(clippy::too_many_arguments)]
    fn clone_thread(&mut self, data: &mut [u8], flags: i64, _stack: i64, ptid: i64, _tls: i64, ctid: i64, frame: i64) -> SysResult<i64> {
        const CLONE_VM: i64 = 0x100;
        const CLONE_SIGHAND: i64 = 0x800;
        const CLONE_PARENT_SETTID: i64 = 0x100000;
        const CLONE_CHILD_CLEARTID: i64 = 0x200000;
        const CLONE_CHILD_SETTID: i64 = 0x1000000;
        if flags & (CLONE_VM | CLONE_SIGHAND) != CLONE_VM | CLONE_SIGHAND || !self.vfork.is_empty() {
            return Err(EINVAL);
        }
        let tid = self.kernel.create_thread();
        for (flag, at) in [(CLONE_PARENT_SETTID, ptid), (CLONE_CHILD_SETTID, ctid)] {
            if flags & flag != 0 {
                mem::write(data, at as u64, &tid.to_le_bytes())?;
            }
        }
        let thread = Process {
            kernel: self.kernel.clone(),
            pid: self.pid,
            tid,
            memory: self.memory.clone(),
            group: self.group.clone(),
            fds: self.fds.share(),
            fs: self.fs.clone(),
            umask: self.umask,
            sigactions: self.sigactions.share(),
            creds: self.creds.clone(),
            signals: Signals { deliver: None, suspended: None, ..self.signals.clone() },
            vfork: Vec::new(),
            pending: None,
            trace: self.trace,
            start: self.start,
            module: self.module.clone(),
            clear_tid: if flags & CLONE_CHILD_CLEARTID != 0 { ctid as u64 } else { 0 },
            started: None,
            main: None,
        };
        crate::thread::spawn(thread, frame as u64)?;
        Ok(tid as i64)
    }

    fn sleep_for(&self, duration: Duration) -> Result<(), (i64, Duration)> {
        const SLICE: Duration = Duration::from_millis(20);
        let deadline = std::time::Instant::now() + duration;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(());
            }
            if self.kernel.peek_signal(self.pid, !self.interrupting()).is_some() || self.group.ended() {
                return Err((EINTR, deadline - now));
            }
            std::thread::sleep((deadline - now).min(SLICE));
        }
    }

    /// The pipe behind `fd`, if it is one, and whether it's non-blocking.
    /// Pipe I/O blocks, so it happens without holding the descriptor lock.
    fn pipe_of(&self, fd: i64, want_write: bool) -> SysResult<Option<(Arc<crate::pipe::Pipe>, bool)>> {
        let fd = self.fds.get(fd)?;
        let nonblock = fd.flags & o::NONBLOCK != 0;
        match &*fd.open.lock().unwrap() {
            Open::Pipe(end) if end.write == want_write => Ok(Some((end.pipe.clone(), nonblock))),
            Open::Pipe(_) => Err(EBADF),
            _ => Ok(None),
        }
    }

    /// The signals that interrupt a blocking call (with EINTR): those this
    /// thread doesn't block and that do something (end the process, or run a
    /// handler), not ignored ones.
    pub(crate) fn interrupting(&self) -> u64 {
        let mut set = 0u64;
        for sig in 1..NSIG as i32 {
            let handled = self.handler(sig as usize) > process::SIG_IGN && self.main.is_some();
            if handled || process::signal_effect(sig, self.handler(sig as usize)).is_some() {
                set |= 1 << (sig - 1);
            }
        }
        set & !self.signals.mask
    }

    /// Whether a blocking call should give up with EINTR.
    pub(crate) fn interrupted(&self) -> impl Fn() -> bool + use<> {
        let (kernel, pid, group, interrupting) = (self.kernel.clone(), self.pid, self.group.clone(), self.interrupting());
        move || kernel.peek_signal(pid, !interrupting).is_some() || group.ended()
    }

    fn read(&mut self, fd: i64, buf: &mut [u8]) -> SysResult<i64> {
        if let Some(s) = self.maybe_socket(fd)? {
            return crate::net::recv(&s, buf);
        }
        if let Some((pipe, nonblock)) = self.pipe_of(fd, false)? {
            return pipe.read(buf, nonblock, &self.interrupted()).map(|n| n as i64);
        }
        let open = self.fds.get(fd)?.open.clone();
        let r = match &mut *open.lock().unwrap() {
            Open::Stdin => std::io::stdin().read(buf),
            Open::File { file, .. } => file.read(buf),
            Open::Dir { .. } => return Err(EISDIR),
            Open::Device(Device::Null) => Ok(0),
            Open::Device(Device::Zero) => {
                buf.fill(0);
                Ok(buf.len())
            }
            Open::Device(Device::Random) => {
                getrandom::fill(buf).map_err(|_| EIO)?;
                Ok(buf.len())
            }
            Open::Stdout | Open::Stderr | Open::Pipe(_) | Open::Socket(_) => return Err(EBADF),
        };
        r.map(|n| n as i64).map_err(|e| fs::errno(&e))
    }

    fn write(&mut self, fd: i64, buf: &[u8]) -> SysResult<i64> {
        if let Some(s) = self.maybe_socket(fd)? {
            return crate::net::send(&s, buf);
        }
        if let Some((pipe, nonblock)) = self.pipe_of(fd, true)? {
            let r = pipe.write(buf, nonblock, &self.interrupted());
            if r == Err(EPIPE) {
                // Writing to a pipe nobody reads raises SIGPIPE.
                const SIGPIPE: i32 = 13;
                if let Some(how) = self.signal_effect(SIGPIPE) {
                    self.pending = Some(Pending::Terminate(how));
                }
            }
            return r.map(|n| n as i64);
        }
        let open = self.fds.get(fd)?.open.clone();
        let r = match &mut *open.lock().unwrap() {
            Open::Stdout => {
                let mut out = std::io::stdout().lock();
                out.write_all(buf).and_then(|_| out.flush())
            }
            Open::Stderr => std::io::stderr().write_all(buf),
            Open::File { file, append, .. } => {
                if *append {
                    file.seek(SeekFrom::End(0)).map_err(|e| fs::errno(&e))?;
                }
                file.write_all(buf)
            }
            Open::Dir { .. } => return Err(EISDIR),
            Open::Device(_) => Ok(()), // writes to /dev devices are discarded
            Open::Stdin | Open::Pipe(_) | Open::Socket(_) => return Err(EBADF),
        };
        r.map(|_| buf.len() as i64).map_err(|e| fs::errno(&e))
    }

    fn with_file<T>(&self, fd: i64, f: impl FnOnce(&mut std::fs::File) -> std::io::Result<T>) -> SysResult<T> {
        let open = self.fds.get(fd)?.open.clone();
        let mut open = open.lock().unwrap();
        match &mut *open {
            Open::File { file, .. } => f(file).map_err(|e| fs::errno(&e)),
            Open::Dir { .. } => Err(EISDIR),
            _ => Err(ESPIPE),
        }
    }

    fn pread(&mut self, fd: i64, buf: &mut [u8], off: u64) -> SysResult<i64> {
        self.with_file(fd, |file| {
            let pos = file.stream_position()?;
            file.seek(SeekFrom::Start(off))?;
            let n = file.read(buf);
            file.seek(SeekFrom::Start(pos))?;
            n
        })
        .map(|n| n as i64)
    }

    fn pwrite(&mut self, fd: i64, buf: &[u8], off: u64) -> SysResult<i64> {
        self.with_file(fd, |file| {
            let pos = file.stream_position()?;
            file.seek(SeekFrom::Start(off))?;
            let n = file.write(buf);
            file.seek(SeekFrom::Start(pos))?;
            n
        })
        .map(|n| n as i64)
    }

    fn openat(&mut self, dirfd: i64, path: &str, flags: i64) -> SysResult<i64> {
        let dir = self.dir_of(dirfd)?;
        match self.fs.device(&dir, path) {
            Some(DevPath::Device(d)) => {
                let open = Arc::new(Mutex::new(Open::Device(d)));
                return self.fds.insert(Fd { open, cloexec: flags & o::CLOEXEC != 0, flags }, 0);
            }
            Some(DevPath::Fd(n)) => {
                let fd = Fd { cloexec: flags & o::CLOEXEC != 0, ..self.fds.get(n)?.clone() };
                return self.fds.insert(fd, 0);
            }
            Some(DevPath::Synthetic(s)) => {
                if flags & o::ACCMODE != 0 {
                    return Err(EACCES);
                }
                let file = s.open().map_err(|e| fs::errno(&e))?;
                let open = Arc::new(Mutex::new(Open::File { file, path: path.to_string(), append: false }));
                return self.fds.insert(Fd { open, cloexec: flags & o::CLOEXEC != 0, flags }, 0);
            }
            None => {}
        }
        let guest = self.fs.resolve(&dir, path, flags & o::NOFOLLOW == 0)?;
        let host = self.fs.host(&guest);
        let access = flags & o::ACCMODE;
        let meta = std::fs::symlink_metadata(&host).ok();
        if meta.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
            return Err(ELOOP); // O_NOFOLLOW on a symlink
        }
        let open = if meta.as_ref().is_some_and(|m| m.is_dir()) {
            if flags & o::CREAT != 0 && flags & o::EXCL != 0 {
                return Err(EEXIST);
            }
            if access != 0 {
                return Err(EISDIR);
            }
            Open::Dir { path: guest, entries: None, pos: 0 }
        } else {
            if flags & o::DIRECTORY != 0 {
                return Err(if meta.is_some() { ENOTDIR } else { ENOENT });
            }
            let mut opts = std::fs::OpenOptions::new();
            opts.read(access != o::WRONLY)
                .write(access != 0)
                .append(flags & o::APPEND != 0)
                .truncate(flags & o::TRUNC != 0 && access != 0);
            if flags & o::CREAT != 0 {
                if flags & o::EXCL != 0 {
                    opts.create_new(true);
                } else {
                    opts.create(true);
                }
                // A file created read-only still needs a write handle on
                // some hosts; Linux allows O_CREAT|O_RDONLY.
                if access == 0 {
                    opts.write(true);
                }
            }
            let file = opts.open(&host).map_err(|e| fs::errno(&e))?;
            Open::File { file, path: guest, append: flags & o::APPEND != 0 }
        };
        let fd = Fd { open: Arc::new(Mutex::new(open)), cloexec: flags & o::CLOEXEC != 0, flags };
        self.fds.insert(fd, 0)
    }

    fn lseek(&mut self, fd: i64, off: i64, whence: i64) -> SysResult<i64> {
        let open = self.fds.get(fd)?.open.clone();
        let mut open = open.lock().unwrap();
        match &mut *open {
            Open::File { file, .. } => {
                let to = match whence {
                    0 => SeekFrom::Start(u64::try_from(off).map_err(|_| EINVAL)?),
                    1 => SeekFrom::Current(off),
                    2 => SeekFrom::End(off),
                    _ => return Err(EINVAL),
                };
                file.seek(to).map(|p| p as i64).map_err(|e| fs::errno(&e))
            }
            Open::Dir { entries, pos, .. } => {
                if whence != 0 || off < 0 {
                    return Err(EINVAL);
                }
                if off == 0 {
                    *entries = None; // rewinddir re-reads the directory
                }
                *pos = off as usize;
                Ok(off)
            }
            Open::Device(_) => Ok(0),
            _ => Err(ESPIPE),
        }
    }

    fn getdents64(&mut self, fd: i64, buf: &mut [u8]) -> SysResult<i64> {
        let open = self.fds.get(fd)?.open.clone();
        let mut open = open.lock().unwrap();
        let Open::Dir { path, entries, pos } = &mut *open else { return Err(ENOTDIR) };
        if entries.is_none() {
            *entries = Some(fs::read_dir(&self.fs, path)?);
        }
        let entries = entries.as_ref().unwrap();
        let mut used = 0;
        while let Some(e) = entries.get(*pos) {
            // struct linux_dirent64: ino, off, reclen, type, name, NUL.
            let reclen = (19 + e.name.len() + 1).div_ceil(8) * 8;
            if used + reclen > buf.len() {
                if used == 0 {
                    return Err(EINVAL);
                }
                break;
            }
            let rec = &mut buf[used..used + reclen];
            rec.fill(0);
            rec[0..8].copy_from_slice(&e.ino.to_le_bytes());
            rec[8..16].copy_from_slice(&((*pos + 1) as i64).to_le_bytes());
            rec[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
            rec[18] = e.dtype;
            rec[19..19 + e.name.len()].copy_from_slice(e.name.as_bytes());
            used += reclen;
            *pos += 1;
        }
        Ok(used as i64)
    }

    fn fstat(&mut self, fd: i64) -> SysResult<[u8; 128]> {
        let open = self.fds.get(fd)?.open.clone();
        let open = open.lock().unwrap();
        match &*open {
            Open::File { file, .. } => file.metadata().map(|m| fs::stat_bytes(&m)).map_err(|e| fs::errno(&e)),
            Open::Dir { path, .. } => {
                std::fs::metadata(self.fs.host(path)).map(|m| fs::stat_bytes(&m)).map_err(|e| fs::errno(&e))
            }
            Open::Pipe(_) => Ok(fs::fifo_stat_bytes()),
            Open::Device(d) => Ok(fs::device_stat_bytes(*d)),
            _ => Ok(fs::stdio_stat_bytes()),
        }
    }

    /// `struct statfs` for the environment's filesystem: the host's figures
    /// for the root directory, reported as ext4.
    fn statfs(&self) -> [u8; 120] {
        let mut b = [0u8; 120];
        let mut words = [0xef53u64, 4096, 0, 0, 0, 0, 0]; // type, bsize, blocks, bfree, bavail, files, ffree
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            if let Ok(path) = std::ffi::CString::new(self.fs.host("/").as_os_str().as_bytes()) {
                // SAFETY: statvfs fills the struct it's given.
                let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
                if unsafe { libc::statvfs(path.as_ptr(), &mut s) } == 0 {
                    words = [0xef53, s.f_bsize, s.f_blocks, s.f_bfree, s.f_bavail, s.f_files, s.f_ffree];
                }
            }
        }
        for (i, w) in words.iter().enumerate() {
            b[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        b[64..72].copy_from_slice(&255u64.to_le_bytes()); // f_namelen
        b[72..80].copy_from_slice(&words[1].to_le_bytes()); // f_frsize
        b
    }

    fn fstatat(&mut self, dirfd: i64, path: &str, flags: i64) -> SysResult<[u8; 128]> {
        if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
            return self.fstat(dirfd);
        }
        let dir = self.dir_of(dirfd)?;
        match self.fs.device(&dir, path) {
            Some(DevPath::Device(d)) => return Ok(fs::device_stat_bytes(d)),
            Some(DevPath::Fd(n)) => return self.fstat(n),
            Some(DevPath::Synthetic(s)) => {
                let meta = s.open().and_then(|f| f.metadata()).map_err(|e| fs::errno(&e))?;
                return Ok(fs::stat_bytes(&meta));
            }
            None => {}
        }
        let guest = self.fs.resolve(&dir, path, flags & AT_SYMLINK_NOFOLLOW == 0)?;
        let meta = std::fs::symlink_metadata(self.fs.host(&guest)).map_err(|e| fs::errno(&e))?;
        Ok(fs::stat_bytes(&meta))
    }

    /// fcntl's record locks (F_GETLK, F_SETLK, F_SETLKW and their F_OFD_
    /// forms) on the file behind `fd`: the host's open-file-description
    /// locks, since the environment's processes share one host process (so
    /// its per-process POSIX locks would never conflict between them). A
    /// lock on anything but a file succeeds.
    fn lock(&mut self, data: &mut [u8], fd: i64, cmd: i64, flock: u64) -> SysResult<i64> {
        let open = self.fds.get(fd)?.open.clone();
        let guard = open.lock().unwrap();
        let Open::File { file, .. } = &*guard else { return Ok(0) };
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            // struct flock: l_type, l_whence (i16), l_start, l_len (i64),
            // l_pid (i32): the same layout on every 64-bit Linux.
            let b = mem::slice(data, flock, 32)?;
            let mut l: libc::flock = unsafe { std::mem::zeroed() };
            l.l_type = i16::from_le_bytes([b[0], b[1]]);
            l.l_whence = i16::from_le_bytes([b[2], b[3]]);
            l.l_start = i64::from_le_bytes(b[8..16].try_into().unwrap());
            l.l_len = i64::from_le_bytes(b[16..24].try_into().unwrap());
            let host_cmd = match cmd {
                5 | 36 => libc::F_OFD_GETLK,
                6 | 37 => libc::F_OFD_SETLK,
                _ => libc::F_OFD_SETLKW,
            };
            // SAFETY: a valid descriptor and a flock struct.
            let r = unsafe { libc::fcntl(file.as_raw_fd(), host_cmd, &mut l) };
            if r < 0 {
                return Err(std::io::Error::last_os_error().raw_os_error().map_or(EIO, |e| e as i64));
            }
            if host_cmd == libc::F_OFD_GETLK {
                let b = mem::slice_mut(data, flock, 32)?;
                b[0..2].copy_from_slice(&l.l_type.to_le_bytes());
                b[2..4].copy_from_slice(&l.l_whence.to_le_bytes());
                b[8..16].copy_from_slice(&l.l_start.to_le_bytes());
                b[16..24].copy_from_slice(&l.l_len.to_le_bytes());
                b[24..28].copy_from_slice(&(if l.l_type == libc::F_UNLCK as i16 { 0 } else { -1i32 }).to_le_bytes());
            }
        }
        let _ = (data, cmd, flock, file);
        Ok(0)
    }

    /// flock: the host's (a lock per open file description, as on Linux).
    fn flock(&mut self, fd: i64, op: i64) -> SysResult<i64> {
        let open = self.fds.get(fd)?.open.clone();
        let guard = open.lock().unwrap();
        let Open::File { file, .. } = &*guard else { return Ok(0) };
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: a valid descriptor.
            if unsafe { libc::flock(file.as_raw_fd(), op as i32) } < 0 {
                return Err(std::io::Error::last_os_error().raw_os_error().map_or(EIO, |e| e as i64));
            }
        }
        let _ = (file, op);
        Ok(0)
    }

    fn fcntl(&mut self, fd: i64, cmd: i64, arg: i64) -> SysResult<i64> {
        // fcntl is variadic; its argument is an int, so the high 32 bits
        // of the register are undefined. Linux ignores them too.
        let arg = arg as i32 as i64;
        const F_DUPFD: i64 = 0;
        const F_GETFD: i64 = 1;
        const F_SETFD: i64 = 2;
        const F_GETFL: i64 = 3;
        const F_SETFL: i64 = 4;
        const F_DUPFD_CLOEXEC: i64 = 1030;
        match cmd {
            F_DUPFD | F_DUPFD_CLOEXEC => {
                let fd = Fd { cloexec: cmd == F_DUPFD_CLOEXEC, ..self.fds.get(fd)?.clone() };
                self.fds.insert(fd, usize::try_from(arg).map_err(|_| EINVAL)?)
            }
            F_GETFD => Ok(self.fds.get(fd)?.cloexec as i64),
            F_SETFD => {
                self.fds.update(fd, |fd| fd.cloexec = arg & 1 != 0)?;
                Ok(0)
            }
            F_GETFL => Ok(self.fds.get(fd)?.flags & !o::CLOEXEC),
            F_SETFL => {
                self.fds.update(fd, |fd| fd.flags = (fd.flags & o::ACCMODE) | (arg & !o::ACCMODE))?;
                Ok(0)
            }
            _ => Err(EINVAL),
        }
    }

    fn ioctl(&mut self, data: &mut [u8], fd: i64, req: i64, arg: u64) -> SysResult<i64> {
        // The request is an unsigned int (see fcntl).
        let req = req as u32 as i64;
        const TIOCGWINSZ: i64 = 0x5413;
        const FIONREAD: i64 = 0x541b;
        const FIONBIO: i64 = 0x5421;
        if req == FIONBIO {
            // Set or clear O_NONBLOCK (Python's socket.settimeout does this).
            let on = u32::from_le_bytes(mem::slice(data, arg, 4)?.try_into().unwrap()) != 0;
            self.fds.update(fd, |fd| fd.flags = if on { fd.flags | o::NONBLOCK } else { fd.flags & !o::NONBLOCK })?;
            return Ok(0);
        }
        if req == FIONREAD
            && let Some(s) = self.maybe_socket(fd)?
        {
            let n = crate::net::pending(&s)?;
            return mem::write(data, arg, &(n as i32).to_le_bytes()).map(|_| 0);
        }
        if req == FIONREAD
            && let Some((pipe, _)) = self.pipe_of(fd, false)? {
                return mem::write(data, arg, &(pipe.available() as i32).to_le_bytes()).map(|_| 0);
            }
        // The host descriptor behind the guest's, when it's a terminal.
        let host_fd = match &*self.fds.get(fd)?.open.lock().unwrap() {
            Open::Stdin if std::io::stdin().is_terminal() => Some(0),
            Open::Stdout if std::io::stdout().is_terminal() => Some(1),
            Open::Stderr if std::io::stderr().is_terminal() => Some(2),
            _ => None,
        };
        let Some(host_fd) = host_fd else { return Err(ENOTTY) };
        const TCGETS: i64 = 0x5401;
        const TCSETS: i64 = 0x5402;
        const TCSETSW: i64 = 0x5403;
        const TCSETSF: i64 = 0x5404;
        const TCXONC: i64 = 0x540a;
        const TCFLSH: i64 = 0x540b;
        const TIOCSCTTY: i64 = 0x540e;
        const TIOCGPGRP: i64 = 0x540f;
        const TIOCSPGRP: i64 = 0x5410;
        const TIOCNOTTY: i64 = 0x5422;
        const TIOCGSID: i64 = 0x5429;
        use std::sync::atomic::Ordering::Relaxed;
        match req {
            // Terminal settings, window size and flow control are the host
            // terminal's (struct termios and winsize have the same layout on
            // every Linux; other hosts would convert them).
            TCGETS | TCSETS | TCSETSW | TCSETSF | TIOCGWINSZ | TCXONC | TCFLSH => {
                const TERMIOS: usize = 60; // struct termios (36 bytes) and winsize fit
                let mut buf = [0u8; TERMIOS];
                let takes_struct = matches!(req, TCGETS | TCSETS | TCSETSW | TCSETSF | TIOCGWINSZ);
                if matches!(req, TCSETS | TCSETSW | TCSETSF) {
                    buf[..36].copy_from_slice(mem::slice(data, arg, 36)?);
                }
                host_tty_ioctl(host_fd, req, if takes_struct { buf.as_mut_ptr() as u64 } else { arg })?;
                match req {
                    TCGETS => mem::write(data, arg, &buf[..36]).map(|_| 0),
                    TIOCGWINSZ => mem::write(data, arg, &buf[..8]).map(|_| 0),
                    _ => Ok(0),
                }
            }
            // Job control: the foreground process group is the runtime's.
            TIOCGPGRP => mem::write(data, arg, &self.kernel.foreground.load(Relaxed).to_le_bytes()).map(|_| 0),
            TIOCSPGRP => {
                let pgrp = i32::from_le_bytes(mem::slice(data, arg, 4)?.try_into().unwrap());
                self.kernel.foreground.store(pgrp, Relaxed);
                Ok(0)
            }
            TIOCGSID => {
                let sid = self.kernel.sid(self.pid)?;
                mem::write(data, arg, &sid.to_le_bytes()).map(|_| 0)
            }
            TIOCSCTTY | TIOCNOTTY => Ok(0),
            _ => Err(ENOTTY),
        }
    }

    fn clock(&self, id: i64) -> SysResult<Duration> {
        const REALTIME: i64 = 0;
        const REALTIME_COARSE: i64 = 5;
        match id {
            REALTIME | REALTIME_COARSE => Ok(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()),
            // Monotonic, CPU time and boot time all count from process start.
            1..=7 => Ok(self.start.elapsed()),
            _ => Err(EINVAL),
        }
    }
}

/// A terminal ioctl on host descriptor `fd` (0–2), passing `arg` through.
#[cfg(target_os = "linux")]
fn host_tty_ioctl(fd: i32, req: i64, arg: u64) -> SysResult<()> {
    // SAFETY: `req` is one of the terminal requests ioctl() checked, whose
    // argument is a value or a buffer of the kernel's struct (at least as
    // large as the caller's).
    let r = unsafe { libc::ioctl(fd, req as libc::Ioctl, arg as usize) };
    if r < 0 { Err(std::io::Error::last_os_error().raw_os_error().map_or(EIO, |e| e as i64)) } else { Ok(()) }
}

#[cfg(not(target_os = "linux"))]
fn host_tty_ioctl(_fd: i32, _req: i64, _arg: u64) -> SysResult<()> {
    Err(ENOTTY)
}

/// Sets `ids` ([real, effective, saved]) to `new`, where -1 leaves one
/// unchanged, as the set*id calls do (ids are 32 bits).
fn set_ids(ids: &mut [u32; 3], new: [i64; 3]) -> i64 {
    for (id, v) in ids.iter_mut().zip(new) {
        if v as i32 != -1 {
            *id = v as u32;
        }
    }
    0
}

/// The CPUs the guest sees: the host's.
pub fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

const CLONE_THREAD: i64 = 0x10000;

fn timespec(t: Duration) -> [u8; 16] {
    let mut ts = [0u8; 16];
    ts[..8].copy_from_slice(&(t.as_secs() as i64).to_le_bytes());
    ts[8..].copy_from_slice(&(t.subsec_nanos() as i64).to_le_bytes());
    ts
}

#[cfg(unix)]
fn symlink(target: &str, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &str, link: &std::path::Path) -> std::io::Result<()> {
    // Windows needs to know the target's type; guest targets may be
    // absolute guest paths, so assume a file.
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn chmod(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
}

#[cfg(windows)]
fn chmod(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    // Windows only has a read-only flag.
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_readonly(mode & 0o222 == 0);
    std::fs::set_permissions(path, perms)
}

/// Reads a NULL-terminated array of string pointers (argv, envp).
fn string_array(data: &[u8], mut addr: u64) -> SysResult<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    if addr == 0 {
        return Ok(out);
    }
    loop {
        let p = mem::read_u64(data, addr)?;
        if p == 0 {
            return Ok(out);
        }
        if out.len() > 100_000 {
            return Err(abi::errno::E2BIG);
        }
        out.push(mem::cstr_bytes(data, p)?);
        addr += 8;
    }
}

/// Reads a struct timespec, rejecting invalid values like the kernel does.
fn read_timespec(data: &[u8], addr: u64) -> SysResult<Duration> {
    let ts = mem::slice(data, addr, 16)?;
    let secs = i64::from_le_bytes(ts[..8].try_into().unwrap());
    let nanos = i64::from_le_bytes(ts[8..].try_into().unwrap());
    if secs < 0 || !(0..1_000_000_000).contains(&nanos) {
        return Err(EINVAL);
    }
    Ok(Duration::new(secs as u64, nanos as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timespecs() {
        let mut data = vec![0u8; 32];
        mem::write(&mut data, 0, &timespec(Duration::new(3, 500))).unwrap();
        assert_eq!(read_timespec(&data, 0), Ok(Duration::new(3, 500)), "round trip");
        mem::write(&mut data, 16, &(-1i64).to_le_bytes()).unwrap();
        assert_eq!(read_timespec(&data, 16), Err(EINVAL), "negative seconds");
        mem::write(&mut data, 16, &0i64.to_le_bytes()).unwrap();
        mem::write(&mut data, 24, &1_000_000_000i64.to_le_bytes()).unwrap();
        assert_eq!(read_timespec(&data, 16), Err(EINVAL), "nanoseconds out of range");
        assert_eq!(read_timespec(&data, 24), Err(EFAULT));
    }

    #[test]
    fn argv_arrays() {
        let mut data = vec![0u8; 128];
        data[64..70].copy_from_slice(b"hello\0");
        data[70..73].copy_from_slice(&[0x80, 0x81, 0]);
        // The array lives at address 8 (address 0 is NULL).
        for (i, p) in [64u64, 70, 0].iter().enumerate() {
            mem::write_u64(&mut data, 8 + i as u64 * 8, *p).unwrap();
        }
        assert_eq!(string_array(&data, 8), Ok(vec![b"hello".to_vec(), vec![0x80, 0x81]]));
        assert_eq!(string_array(&data, 24), Ok(vec![]), "empty array");
        assert_eq!(string_array(&data, 0), Ok(vec![]), "NULL array pointer");
        assert_eq!(string_array(&data, 124), Err(EFAULT), "array runs past memory");
    }
}
