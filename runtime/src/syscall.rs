//! The `linux.syscall` import: Linux syscalls (generic/arm64 numbering) on
//! top of the host. See docs/abi.md.

use std::cell::RefCell;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasmtime::Caller;

use crate::abi::{self, AT_EMPTY_PATH, AT_FDCWD, AT_REMOVEDIR, AT_SYMLINK_NOFOLLOW, errno::*, nr, o};
use crate::fs::{self, Fd, Open};
use crate::memory::{self as mem, SysResult, page_align};
use crate::process::{Exit, Process};

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
    let result = match n {
        nr::EXIT | nr::EXIT_GROUP => Err(Exit(a as i32 & 0xff)),
        // A signal sent to ourselves (abort() raises SIGABRT) terminates
        // the process with the shell's 128+signal status.
        nr::KILL | nr::TGKILL => match signal_self(n, &args) {
            Some(sig) => Err(Exit(128 + sig)),
            None => Ok(Ok(0)),
        },
        nr::MMAP => Ok(mmap(&mut caller, &args)),
        nr::MREMAP => Ok(mremap(&mut caller, &args)),
        _ => {
            let memory = caller.data().memory.expect("memory is set before _start");
            let (data, process) = memory.data_and_store_mut(&mut caller);
            Ok(process.call(data, n, &args))
        }
    };
    match result {
        Ok(r) => {
            let ret = r.unwrap_or_else(|e| -e);
            if caller.data().trace {
                eprintln!("[wasm-env] {}({a:#x}, {b:#x}, {c:#x}) = {ret}", abi::name(n));
            }
            Ok(ret)
        }
        Err(exit) => {
            if caller.data().trace {
                eprintln!("[wasm-env] {}({a}) -> exit {}", abi::name(n), exit.0);
            }
            Err(wasmtime::Error::new(exit))
        }
    }
}

/// The signal number if this kill/tgkill targets our own process.
fn signal_self(n: i64, args: &[i64; 6]) -> Option<i32> {
    let (pid, sig) = if n == nr::KILL { (args[0], args[1]) } else { (args[0], args[2]) };
    (sig != 0 && (pid == 1 || pid == 0 || pid == -1)).then_some(sig as i32)
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
        let mut open = open.borrow_mut();
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
                let Open::Dir { path, .. } = &*fd.open.borrow() else { return Err(ENOTDIR) };
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
                let path = match &*open.borrow() {
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
            nr::SET_TID_ADDRESS | nr::GETPID | nr::GETTID => Ok(1),
            nr::GETPPID | nr::GETUID | nr::GETEUID | nr::GETGID | nr::GETEGID => Ok(0),
            nr::SET_ROBUST_LIST | nr::SIGALTSTACK | nr::MPROTECT | nr::MADVISE => Ok(0),
            nr::RT_SIGACTION => {
                // No signal delivery yet: report every action as default.
                if c != 0 {
                    mem::slice_mut(data, uc, 32)?.fill(0);
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
            nr::NANOSLEEP => {
                let ts = mem::slice(data, ua, 16)?;
                let secs = i64::from_le_bytes(ts[..8].try_into().unwrap());
                let nanos = i64::from_le_bytes(ts[8..].try_into().unwrap());
                if secs < 0 || !(0..1_000_000_000).contains(&nanos) {
                    return Err(EINVAL);
                }
                std::thread::sleep(Duration::new(secs as u64, nanos as u32));
                Ok(0)
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
        match &*fd.open.borrow() {
            Open::Dir { path, .. } => Ok(path.clone()),
            _ => Err(ENOTDIR),
        }
    }

    fn read(&mut self, fd: i64, buf: &mut [u8]) -> SysResult<i64> {
        let open = self.fds.get(fd)?.open.clone();
        let r = match &mut *open.borrow_mut() {
            Open::Stdin => std::io::stdin().read(buf),
            Open::File { file, .. } => file.read(buf),
            Open::Dir { .. } => return Err(EISDIR),
            Open::Stdout | Open::Stderr => return Err(EBADF),
        };
        r.map(|n| n as i64).map_err(|e| fs::errno(&e))
    }

    fn write(&mut self, fd: i64, buf: &[u8]) -> SysResult<i64> {
        let open = self.fds.get(fd)?.open.clone();
        let r = match &mut *open.borrow_mut() {
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
            Open::Stdin => return Err(EBADF),
        };
        r.map(|_| buf.len() as i64).map_err(|e| fs::errno(&e))
    }

    fn with_file<T>(&self, fd: i64, f: impl FnOnce(&mut std::fs::File) -> std::io::Result<T>) -> SysResult<T> {
        let open = self.fds.get(fd)?.open.clone();
        let mut open = open.borrow_mut();
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
        let fd = Fd { open: Rc::new(RefCell::new(open)), cloexec: flags & o::CLOEXEC != 0, flags };
        self.fds.insert(fd, 0)
    }

    fn lseek(&mut self, fd: i64, off: i64, whence: i64) -> SysResult<i64> {
        let open = self.fds.get(fd)?.open.clone();
        let mut open = open.borrow_mut();
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
            _ => Err(ESPIPE),
        }
    }

    fn getdents64(&mut self, fd: i64, buf: &mut [u8]) -> SysResult<i64> {
        let open = self.fds.get(fd)?.open.clone();
        let mut open = open.borrow_mut();
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
        let open = open.borrow();
        match &*open {
            Open::File { file, .. } => file.metadata().map(|m| fs::stat_bytes(&m)).map_err(|e| fs::errno(&e)),
            Open::Dir { path, .. } => {
                std::fs::metadata(self.fs.host(path)).map(|m| fs::stat_bytes(&m)).map_err(|e| fs::errno(&e))
            }
            _ => Ok(fs::stdio_stat_bytes()),
        }
    }

    fn fstatat(&mut self, dirfd: i64, path: &str, flags: i64) -> SysResult<[u8; 128]> {
        if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
            return self.fstat(dirfd);
        }
        let dir = self.dir_of(dirfd)?;
        let guest = self.fs.resolve(&dir, path, flags & AT_SYMLINK_NOFOLLOW == 0)?;
        let meta = std::fs::symlink_metadata(self.fs.host(&guest)).map_err(|e| fs::errno(&e))?;
        Ok(fs::stat_bytes(&meta))
    }

    fn fcntl(&mut self, fd: i64, cmd: i64, arg: i64) -> SysResult<i64> {
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
        const TIOCGWINSZ: i64 = 0x5413;
        let tty = match &*self.fds.get(fd)?.open.borrow() {
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
