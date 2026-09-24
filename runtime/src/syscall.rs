//! The `linux.syscall` import: Linux syscalls (generic/arm64 numbering) on
//! top of the host. See docs/abi.md.

use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasmtime::Caller;

use crate::abi::{self, AT_EMPTY_PATH, AT_FDCWD, AT_REMOVEDIR, AT_SYMLINK_NOFOLLOW, errno::*, nr, o};
use crate::fs::{self, DevPath, Device, Fd, Open};
use crate::kernel::Termination;
use crate::memory::{self as mem, SysResult, page_align};
use crate::process::{self, NSIG, Pending, Process, Unwind, VforkFrame};

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
    deliver_signal(caller.data_mut())?;
    let result = match n {
        nr::MMAP => mmap(&mut caller, &args),
        nr::MREMAP => mremap(&mut caller, &args),
        _ => {
            let memory = caller.data().memory.expect("memory is set before _start");
            let (data, process) = memory.data_and_store_mut(&mut caller);
            process.call(data, n, &args)
        }
    };
    let ret = result.unwrap_or_else(|e| -e);
    let process = caller.data_mut();
    if process.trace {
        eprintln!("[wasm-env] pid {}: {}({a:#x}, {b:#x}, {c:#x}) = {ret}", process.pid, abi::name(n));
    }
    if process.pending.is_some() {
        return Err(wasmtime::Error::new(Unwind));
    }
    deliver_signal(process)?;
    Ok(ret)
}

/// Acts on a signal sent to this process: its default action ends the
/// process (unless the signal is ignored).
fn deliver_signal(p: &mut Process) -> wasmtime::Result<()> {
    if let Some(sig) = p.kernel.take_signal(p.pid)
        && let Some(how) = p.signal_effect(sig) {
            p.pending = Some(Pending::Terminate(how));
            return Err(wasmtime::Error::new(Unwind));
        }
    Ok(())
}

const MAP_FIXED: i64 = 0x10;
const MAP_ANONYMOUS: i64 = 0x20;
const MREMAP_MAYMOVE: i64 = 1;

/// Grows linear memory so it covers the whole mmap region.
fn cover(caller: &mut Caller<'_, Process>) -> SysResult<()> {
    let memory = caller.data().memory.unwrap();
    let top = caller.data().space.top;
    let size = memory.data_size(&*caller) as u64;
    if top > size {
        let pages = (top - size).div_ceil(65536);
        memory.grow(&mut *caller, pages).map_err(|_| ENOMEM)?;
    }
    Ok(())
}

fn mmap(caller: &mut Caller<'_, Process>, args: &[i64; 6]) -> SysResult<i64> {
    let [addr, len, _prot, flags, fd, off] = *args;
    if len <= 0 {
        return Err(EINVAL);
    }
    let len = page_align(len as u64);
    let space = &mut caller.data_mut().space;
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
    cover(caller)?;
    let memory = caller.data().memory.unwrap();
    let (data, process) = memory.data_and_store_mut(&mut *caller);
    let range = mem::slice_mut(data, addr, len)?;
    range.fill(0);
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

fn mremap(caller: &mut Caller<'_, Process>, args: &[i64; 6]) -> SysResult<i64> {
    let [old, old_len, new_len, flags, ..] = *args;
    if flags & MREMAP_MAYMOVE == 0 || new_len <= 0 {
        return Err(ENOMEM);
    }
    let (old, old_len, new_len) = (old as u64, page_align(old_len as u64), page_align(new_len as u64));
    if !caller.data().space.contains(old, old_len) {
        return Err(EFAULT);
    }
    let new = caller.data_mut().space.alloc(new_len);
    cover(caller)?;
    let memory = caller.data().memory.unwrap();
    let (data, process) = memory.data_and_store_mut(&mut *caller);
    let keep = old_len.min(new_len) as usize;
    data.copy_within(old as usize..old as usize + keep, new as usize);
    data[new as usize + keep..(new + new_len) as usize].fill(0);
    process.space.release(old, old_len);
    Ok(new as i64)
}

impl Process {
    /// Handles syscalls that only need access to memory, not the store.
    fn call(&mut self, data: &mut [u8], n: i64, args: &[i64; 6]) -> SysResult<i64> {
        let [a, b, c, d, ..] = *args;
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
                let mut cwd = self.fs.cwd.clone().into_bytes();
                cwd.push(0);
                if (cwd.len() as u64) > ub {
                    return Err(ERANGE);
                }
                mem::write(data, ua, &cwd).map(|_| cwd.len() as i64)
            }
            nr::CHDIR => {
                let path = mem::cstr(data, ua)?;
                let guest = self.fs.resolve(&self.fs.cwd.clone(), &path, true)?;
                let meta = std::fs::metadata(self.fs.host(&guest)).map_err(|e| fs::errno(&e))?;
                if !meta.is_dir() {
                    return Err(ENOTDIR);
                }
                self.fs.cwd = guest;
                Ok(0)
            }
            nr::FCHDIR => {
                let fd = self.fds.get(a)?;
                let Open::Dir { path, .. } = &*fd.open.lock().unwrap() else { return Err(ENOTDIR) };
                self.fs.cwd = path.clone();
                Ok(0)
            }
            nr::FCNTL => self.fcntl(a, b, c),
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
            nr::FSYNC | nr::FDATASYNC => self.with_file(a, |f| f.sync_all()).map(|_| 0),
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
                let file = std::fs::File::open(self.fs.host(&guest)).map_err(|e| fs::errno(&e))?;
                apply(&file)
            }
            nr::SET_TID_ADDRESS | nr::GETPID | nr::GETTID => Ok(self.pid as i64),
            nr::GETPPID => Ok(self.kernel.ppid(self.pid) as i64),
            nr::GETUID | nr::GETEUID | nr::GETGID | nr::GETEGID => Ok(0),
            nr::EXIT | nr::EXIT_GROUP => Ok(self.exit(a as i32 & 0xff)),
            nr::KILL => self.kernel.kill(self.pid, a as i32, b as i32).map(|_| 0),
            nr::TGKILL => self.kernel.kill(self.pid, a as i32, c as i32).map(|_| 0),
            nr::CLONE => self.clone_vfork(a, b),
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
                match self.kernel.wait(self.pid, a as i32, c & WNOHANG != 0)? {
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
                let fd = |end| Fd {
                    open: Arc::new(Mutex::new(Open::Pipe(end))),
                    cloexec: b & o::CLOEXEC != 0,
                    flags: b & o::NONBLOCK,
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
            nr::SET_ROBUST_LIST | nr::SIGALTSTACK | nr::MPROTECT | nr::MADVISE => Ok(0),
            nr::RT_SIGACTION => {
                // Handlers are recorded and reported back, but not yet run;
                // SIG_IGN takes effect (see Process::signal_effect).
                let sig = usize::try_from(a).ok().filter(|s| (1..NSIG).contains(s)).ok_or(EINVAL)?;
                if c != 0 {
                    mem::write(data, uc, &self.sigactions[sig])?;
                }
                if b != 0 {
                    if sig == 9 || sig == 19 {
                        return Err(EINVAL); // SIGKILL and SIGSTOP can't be changed
                    }
                    self.sigactions[sig].copy_from_slice(mem::slice(data, ub, 32)?);
                }
                Ok(0)
            }
            nr::RT_SIGPROCMASK => {
                if c != 0 {
                    mem::slice_mut(data, uc, (d as u64).min(128))?.fill(0);
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
                // One CPU: the environment is single-threaded.
                mem::slice_mut(data, uc, ub.min(128))?.fill(0);
                mem::write(data, uc, &[1]).map(|_| 8)
            }
            nr::UMASK => Ok(std::mem::replace(&mut self.umask, (a & 0o777) as u32) as i64),
            nr::BRK => Ok(self.brk as i64),
            nr::MUNMAP => {
                let len = page_align(ub);
                if self.space.contains(ua, len) {
                    self.space.release(ua, len);
                }
                Ok(0)
            }
            nr::PRLIMIT64 => {
                if c != 0 {
                    const RLIMIT_STACK: i64 = 3;
                    const RLIMIT_NOFILE: i64 = 7;
                    let (cur, max) = match b {
                        RLIMIT_STACK => (8 << 20, 8 << 20),
                        RLIMIT_NOFILE => (1024, 1024),
                        _ => (u64::MAX, u64::MAX),
                    };
                    mem::write_u64(data, uc, cur)?;
                    mem::write_u64(data, uc + 8, max)?;
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
        if dirfd == AT_FDCWD {
            return Ok(self.fs.cwd.clone());
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
        self.vfork.push(VforkFrame {
            parent_pid: self.pid,
            fds: self.fds.clone(),
            cwd: self.fs.cwd.clone(),
            umask: self.umask,
            sigactions: self.sigactions.clone(),
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
        self.fs.cwd = frame.cwd;
        self.umask = frame.umask;
        self.sigactions = frame.sigactions;
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

    fn sleep_for(&self, duration: Duration) -> Result<(), (i64, Duration)> {
        const SLICE: Duration = Duration::from_millis(20);
        let deadline = std::time::Instant::now() + duration;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(());
            }
            if self.kernel.peek_signal(self.pid).is_some() {
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

    /// Whether a blocking call should give up with EINTR.
    fn interrupted(&self) -> impl Fn() -> bool + use<> {
        let (kernel, pid) = (self.kernel.clone(), self.pid);
        move || kernel.peek_signal(pid).is_some()
    }

    fn read(&mut self, fd: i64, buf: &mut [u8]) -> SysResult<i64> {
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
            Open::Stdout | Open::Stderr | Open::Pipe(_) => return Err(EBADF),
        };
        r.map(|n| n as i64).map_err(|e| fs::errno(&e))
    }

    fn write(&mut self, fd: i64, buf: &[u8]) -> SysResult<i64> {
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
            Open::Stdin | Open::Pipe(_) => return Err(EBADF),
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

    fn fstatat(&mut self, dirfd: i64, path: &str, flags: i64) -> SysResult<[u8; 128]> {
        if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
            return self.fstat(dirfd);
        }
        let dir = self.dir_of(dirfd)?;
        match self.fs.device(&dir, path) {
            Some(DevPath::Device(d)) => return Ok(fs::device_stat_bytes(d)),
            Some(DevPath::Fd(n)) => return self.fstat(n),
            None => {}
        }
        let guest = self.fs.resolve(&dir, path, flags & AT_SYMLINK_NOFOLLOW == 0)?;
        let meta = std::fs::symlink_metadata(self.fs.host(&guest)).map_err(|e| fs::errno(&e))?;
        Ok(fs::stat_bytes(&meta))
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
                self.fds.get_mut(fd)?.cloexec = arg & 1 != 0;
                Ok(0)
            }
            F_GETFL => Ok(self.fds.get(fd)?.flags & !o::CLOEXEC),
            F_SETFL => {
                let fd = self.fds.get_mut(fd)?;
                fd.flags = (fd.flags & o::ACCMODE) | (arg & !o::ACCMODE);
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
        if req == FIONREAD
            && let Some((pipe, _)) = self.pipe_of(fd, false)? {
                return mem::write(data, arg, &(pipe.available() as i32).to_le_bytes()).map(|_| 0);
            }
        let tty = match &*self.fds.get(fd)?.open.lock().unwrap() {
            Open::Stdin => std::io::stdin().is_terminal(),
            Open::Stdout => std::io::stdout().is_terminal(),
            Open::Stderr => std::io::stderr().is_terminal(),
            _ => false,
        };
        match req {
            // struct winsize { rows, cols, xpixel, ypixel }. TODO: real size.
            TIOCGWINSZ if tty => {
                let ws = [24u16, 80, 0, 0].map(u16::to_le_bytes).concat();
                mem::write(data, arg, &ws).map(|_| 0)
            }
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
