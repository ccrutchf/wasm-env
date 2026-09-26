//! The environment's filesystem view and file descriptors.
//!
//! Guest paths live in their own namespace rooted at a host directory.
//! The runtime resolves every path itself, including symlinks, so an
//! absolute link like /bin/sh -> /bin/busybox resolves inside the root and
//! `..` can't climb out of it.

use std::fs::{File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::abi::errno::*;
use crate::memory::SysResult;
use crate::pipe::PipeEnd;

/// Maps a host I/O error to a Linux errno.
pub fn errno(e: &io::Error) -> i64 {
    use io::ErrorKind::*;
    match e.kind() {
        NotFound => ENOENT,
        PermissionDenied => EACCES,
        AlreadyExists => EEXIST,
        NotADirectory => ENOTDIR,
        IsADirectory => EISDIR,
        DirectoryNotEmpty => ENOTEMPTY,
        InvalidInput => EINVAL,
        ReadOnlyFilesystem => EROFS,
        CrossesDevices => EXDEV,
        InvalidFilename => ENAMETOOLONG,
        NotSeekable => ESPIPE,
        OutOfMemory => ENOMEM,
        _ => EIO,
    }
}

pub struct Fs {
    root: PathBuf,
    /// Current directory: a normalized absolute guest path.
    pub cwd: String,
}

impl Fs {
    pub fn new(root: PathBuf) -> Self {
        Fs { root, cwd: "/".into() }
    }

    /// The host path for a resolved guest path.
    pub fn host(&self, guest: &str) -> PathBuf {
        self.root.join(guest.trim_start_matches('/'))
    }

    /// Resolves `path` relative to the guest directory `dir` into a
    /// normalized absolute guest path, following symlinks (including the
    /// last component if `follow_last`).
    pub fn resolve(&self, dir: &str, path: &str, follow_last: bool) -> SysResult<String> {
        if path.is_empty() {
            return Err(ENOENT);
        }
        let mut todo: Vec<String> = Vec::new();
        push_components(&mut todo, path);
        let mut parts: Vec<String> = if path.starts_with('/') { vec![] } else { split(dir) };
        let mut links = 0;
        while let Some(c) = todo.pop() {
            match c.as_str() {
                "" | "." => continue,
                ".." => {
                    parts.pop();
                    continue;
                }
                _ => {}
            }
            parts.push(c);
            // Trailing slashes and dots don't make a component non-final:
            // `mkdir /tmp/a/` creates `a`.
            let is_last = todo.iter().all(|c| c.is_empty() || c == ".");
            if is_last && !follow_last {
                break;
            }
            let host = self.host(&join(&parts));
            let Ok(meta) = std::fs::symlink_metadata(&host) else {
                // A missing component is fine as the last one (e.g. O_CREAT).
                if is_last {
                    break;
                }
                return Err(ENOENT);
            };
            if meta.file_type().is_symlink() {
                links += 1;
                if links > 40 {
                    return Err(ELOOP);
                }
                let target = std::fs::read_link(&host).map_err(|e| errno(&e))?;
                let target = target.to_str().ok_or(EINVAL)?.to_string();
                parts.pop();
                if target.starts_with('/') {
                    parts.clear();
                }
                push_components(&mut todo, &target);
            } else if !is_last && !meta.is_dir() {
                return Err(ENOTDIR);
            }
        }
        Ok(join(&parts))
    }
}

/// Devices the runtime provides under /dev, whatever the root contains.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Device {
    Null,
    Zero,
    Random,
}

#[derive(Debug, PartialEq)]
pub enum DevPath {
    Device(Device),
    /// /dev/stdin, /dev/stdout, /dev/stderr and /dev/fd/N: another descriptor.
    Fd(i64),
    /// A read-only file the runtime generates (/proc, /sys).
    Synthetic(Synthetic),
}

/// Files under /proc and /sys that programs read to learn about the machine,
/// which the runtime generates whatever the root contains.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Synthetic {
    /// /proc/cpuinfo: one arm64 CPU with the features translated programs
    /// may use (matching AT_HWCAP in the translator's support code).
    CpuInfo,
    /// /sys/devices/system/cpu/{possible,online,present}: "0", one CPU.
    CpuList,
    /// /etc/resolv.conf, when the root's is missing or empty (as in a
    /// fresh image): the host's, so the guest resolves names as the host
    /// does, whose sockets it uses (as Docker gives containers).
    ResolvConf,
}

impl Synthetic {
    pub fn content(self) -> std::borrow::Cow<'static, [u8]> {
        let fixed: &'static [u8] = match self {
            Synthetic::CpuInfo => {
                b"processor\t: 0\nBogoMIPS\t: 50.00\nFeatures\t: fp asimd crc32 atomics lrcpc\n\
                  CPU implementer\t: 0x00\nCPU architecture: 8\nCPU variant\t: 0x0\nCPU part\t: 0x000\n\
                  CPU revision\t: 0\n\n"
            }
            Synthetic::CpuList => b"0\n",
            Synthetic::ResolvConf => {
                return match std::fs::read("/etc/resolv.conf") {
                    Ok(host) if !host.is_empty() => host.into(),
                    // No host resolver configuration (Windows, say): public ones.
                    _ => b"nameserver 1.1.1.1\nnameserver 8.8.8.8\n".as_slice().into(),
                };
            }
        };
        fixed.into()
    }

    /// The content as an open host file (an unlinked temporary), so reading,
    /// seeking, stat and mmap work as for any file.
    pub fn open(self) -> std::io::Result<File> {
        use std::io::{Seek, Write};
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("wasm-env-synthetic-{}-{n}", std::process::id()));
        let mut file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        let _ = std::fs::remove_file(&path);
        file.write_all(&self.content())?;
        file.rewind()?;
        Ok(file)
    }
}

impl Fs {
    /// Recognizes the virtual /dev paths (resolved lexically; /dev itself
    /// is not looked up in the root).
    pub fn device(&self, dir: &str, path: &str) -> Option<DevPath> {
        let mut parts = if path.starts_with('/') { vec![] } else { split(dir) };
        for c in path.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                _ => parts.push(c.to_string()),
            }
        }
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        match parts.as_slice() {
            ["proc", "cpuinfo"] => return Some(DevPath::Synthetic(Synthetic::CpuInfo)),
            ["sys", "devices", "system", "cpu", "possible" | "online" | "present"] => {
                return Some(DevPath::Synthetic(Synthetic::CpuList));
            }
            ["etc", "resolv.conf"]
                if std::fs::metadata(self.host("/etc/resolv.conf")).is_ok_and(|m| m.len() == 0)
                    || std::fs::symlink_metadata(self.host("/etc/resolv.conf")).is_err() =>
            {
                return Some(DevPath::Synthetic(Synthetic::ResolvConf));
            }
            _ => {}
        }
        let ["dev", rest @ ..] = parts.as_slice() else { return None };
        match rest {
            ["null"] => Some(DevPath::Device(Device::Null)),
            ["zero"] => Some(DevPath::Device(Device::Zero)),
            ["random"] | ["urandom"] => Some(DevPath::Device(Device::Random)),
            ["stdin"] => Some(DevPath::Fd(0)),
            ["stdout"] => Some(DevPath::Fd(1)),
            ["stderr"] => Some(DevPath::Fd(2)),
            ["fd", n] => n.parse().ok().map(DevPath::Fd),
            _ => None,
        }
    }
}

/// `struct stat` for a /dev device (Linux's major 1 memory devices).
pub fn device_stat_bytes(d: Device) -> [u8; 128] {
    let minor: u64 = match d {
        Device::Null => 3,
        Device::Zero => 5,
        Device::Random => 9,
    };
    let mut b = [0u8; 128];
    b[16..20].copy_from_slice(&(S_IFCHR | 0o666).to_le_bytes());
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[32..40].copy_from_slice(&(1 << 8 | minor).to_le_bytes());
    b[56..60].copy_from_slice(&4096u32.to_le_bytes());
    b
}

/// Pushes path components onto a stack so they pop in order.
fn push_components(todo: &mut Vec<String>, path: &str) {
    for c in path.split('/').rev() {
        todo.push(c.to_string());
    }
}

fn split(path: &str) -> Vec<String> {
    path.split('/').filter(|c| !c.is_empty()).map(String::from).collect()
}

fn join(parts: &[String]) -> String {
    format!("/{}", parts.join("/"))
}

/// Linux file type bits. On Unix hosts the host's mode is used as is.
#[cfg(not(unix))]
const S_IFREG: u32 = 0o100000;
#[cfg(not(unix))]
const S_IFDIR: u32 = 0o040000;
#[cfg(not(unix))]
const S_IFLNK: u32 = 0o120000;
const S_IFCHR: u32 = 0o020000;

/// Linux dirent d_type values.
fn dtype(meta: &Metadata) -> u8 {
    let t = meta.file_type();
    if t.is_dir() {
        4
    } else if t.is_symlink() {
        10
    } else if t.is_file() {
        8
    } else {
        0
    }
}

/// Encodes metadata as arm64 Linux `struct stat` (128 bytes). Everything
/// in the environment is presented as owned by root, like a container.
pub fn stat_bytes(meta: &Metadata) -> [u8; 128] {
    #[cfg(unix)]
    let (dev, ino, mode, nlink, rdev, blksize, blocks) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino(), meta.mode(), meta.nlink() as u32, meta.rdev(), meta.blksize() as u32, meta.blocks())
    };
    #[cfg(not(unix))]
    let (dev, ino, mode, nlink, rdev, blksize, blocks) = {
        let t = meta.file_type();
        let kind = if t.is_dir() { S_IFDIR } else if t.is_symlink() { S_IFLNK } else { S_IFREG };
        let perm = if meta.permissions().readonly() { 0o555 } else { 0o755 };
        (1u64, 0u64, kind | perm, 1u32, 0u64, 4096u32, meta.len().div_ceil(512))
    };
    let time = |t: io::Result<std::time::SystemTime>| {
        let d = t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).unwrap_or_default();
        (d.as_secs() as i64, d.subsec_nanos() as i64)
    };
    let mut b = [0u8; 128];
    let put = |b: &mut [u8; 128], off: usize, bytes: &[u8]| b[off..off + bytes.len()].copy_from_slice(bytes);
    put(&mut b, 0, &dev.to_le_bytes());
    put(&mut b, 8, &ino.to_le_bytes());
    put(&mut b, 16, &mode.to_le_bytes());
    put(&mut b, 20, &nlink.to_le_bytes());
    // uid (24) and gid (28) stay 0.
    put(&mut b, 32, &rdev.to_le_bytes());
    put(&mut b, 48, &(meta.len() as i64).to_le_bytes());
    put(&mut b, 56, &blksize.to_le_bytes());
    put(&mut b, 64, &(blocks as i64).to_le_bytes());
    for (off, t) in [(72, time(meta.accessed())), (88, time(meta.modified())), (104, time(meta.modified()))] {
        put(&mut b, off, &t.0.to_le_bytes());
        put(&mut b, off + 8, &t.1.to_le_bytes());
    }
    b
}

/// `struct stat` for a terminal or pipe on stdio: a character device.
pub fn stdio_stat_bytes() -> [u8; 128] {
    let mut b = [0u8; 128];
    b[16..20].copy_from_slice(&(S_IFCHR | 0o620).to_le_bytes());
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[56..60].copy_from_slice(&4096u32.to_le_bytes());
    b
}

/// `struct stat` for a pipe.
pub fn fifo_stat_bytes() -> [u8; 128] {
    const S_IFIFO: u32 = 0o010000;
    let mut b = [0u8; 128];
    b[16..20].copy_from_slice(&(S_IFIFO | 0o600).to_le_bytes());
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[56..60].copy_from_slice(&4096u32.to_le_bytes());
    b
}

pub struct DirEntry {
    pub ino: u64,
    pub dtype: u8,
    pub name: String,
}

pub enum Open {
    Stdin,
    Stdout,
    Stderr,
    File { file: File, path: String, append: bool },
    Dir { path: String, entries: Option<Vec<DirEntry>>, pos: usize },
    Pipe(PipeEnd),
    Socket(std::sync::Arc<crate::net::GuestSocket>),
    Device(Device),
}

/// A descriptor: the open file description (shared by dup, vfork and
/// exec, possibly across processes) plus per-descriptor flags.
#[derive(Clone)]
pub struct Fd {
    pub open: Arc<Mutex<Open>>,
    pub cloexec: bool,
    /// File status flags (O_APPEND etc.) as the guest set them.
    pub flags: i64,
}

/// A process's descriptor table. Cloning it (for a vfork child or a new
/// process) shares the open file descriptions, as fork does on Linux.
#[derive(Clone)]
pub struct FdTable {
    fds: Vec<Option<Fd>>,
}

impl FdTable {
    pub fn with_stdio() -> Self {
        let fd = |o| Some(Fd { open: Arc::new(Mutex::new(o)), cloexec: false, flags: 0 });
        FdTable { fds: vec![fd(Open::Stdin), fd(Open::Stdout), fd(Open::Stderr)] }
    }

    pub fn get(&self, fd: i64) -> SysResult<&Fd> {
        usize::try_from(fd).ok().and_then(|i| self.fds.get(i)?.as_ref()).ok_or(EBADF)
    }

    pub fn get_mut(&mut self, fd: i64) -> SysResult<&mut Fd> {
        usize::try_from(fd).ok().and_then(|i| self.fds.get_mut(i)?.as_mut()).ok_or(EBADF)
    }

    /// Installs `fd` at the lowest free number >= `min`.
    pub fn insert(&mut self, fd: Fd, min: usize) -> SysResult<i64> {
        let i = (min..).find(|&i| self.fds.get(i).is_none_or(|f| f.is_none())).unwrap();
        if i >= 1024 {
            return Err(EMFILE);
        }
        if i >= self.fds.len() {
            self.fds.resize(i + 1, None);
        }
        self.fds[i] = Some(fd);
        Ok(i as i64)
    }

    pub fn set(&mut self, n: usize, fd: Fd) {
        if n >= self.fds.len() {
            self.fds.resize(n + 1, None);
        }
        self.fds[n] = Some(fd);
    }

    pub fn close(&mut self, fd: i64) -> SysResult<()> {
        let slot = usize::try_from(fd).ok().and_then(|i| self.fds.get_mut(i)).ok_or(EBADF)?;
        slot.take().map(|_| ()).ok_or(EBADF)
    }

    /// Closes descriptors marked close-on-exec (at execve).
    pub fn close_on_exec(&mut self) {
        for slot in &mut self.fds {
            if slot.as_ref().is_some_and(|f| f.cloexec) {
                *slot = None;
            }
        }
    }
}

/// Reads a directory's entries, including "." and "..".
pub fn read_dir(fs: &Fs, guest: &str) -> SysResult<Vec<DirEntry>> {
    let host = fs.host(guest);
    let ino = |m: &Metadata| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            m.ino()
        }
        #[cfg(not(unix))]
        {
            let _ = m;
            1
        }
    };
    let this = std::fs::metadata(&host).map_err(|e| errno(&e))?;
    let parent = std::fs::metadata(host.parent().unwrap_or(Path::new("/"))).unwrap_or(this.clone());
    let mut entries = vec![
        DirEntry { ino: ino(&this), dtype: 4, name: ".".into() },
        DirEntry { ino: ino(&parent), dtype: 4, name: "..".into() },
    ];
    for e in std::fs::read_dir(&host).map_err(|e| errno(&e))? {
        let e = e.map_err(|e| errno(&e))?;
        let Ok(meta) = e.path().symlink_metadata() else { continue };
        let Some(name) = e.file_name().to_str().map(String::from) else { continue };
        entries.push(DirEntry { ino: ino(&meta), dtype: dtype(&meta), name });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn null_fd() -> Fd {
        Fd { open: Arc::new(Mutex::new(Open::Device(Device::Null))), cloexec: false, flags: 0 }
    }

    #[test]
    fn fd_table_allocates_the_lowest_free_number() {
        let mut t = FdTable::with_stdio();
        assert_eq!(t.insert(null_fd(), 0), Ok(3));
        assert_eq!(t.insert(null_fd(), 0), Ok(4));
        t.close(3).unwrap();
        assert_eq!(t.insert(null_fd(), 0), Ok(3), "reuses the hole");
        assert_eq!(t.insert(null_fd(), 10), Ok(10), "F_DUPFD minimum");
        assert_eq!(t.insert(null_fd(), 5), Ok(5));
        assert_eq!(t.insert(null_fd(), 1024).map_err(|e| e == EMFILE), Err(true), "limit is 1024");
    }

    #[test]
    fn fd_table_errors() {
        let mut t = FdTable::with_stdio();
        assert_eq!(t.get(7).err(), Some(EBADF));
        assert_eq!(t.get(-1).err(), Some(EBADF));
        assert_eq!(t.close(7), Err(EBADF));
        t.close(1).unwrap();
        assert_eq!(t.close(1), Err(EBADF), "double close");
    }

    #[test]
    fn fd_table_close_on_exec() {
        let mut t = FdTable::with_stdio();
        let kept = t.insert(null_fd(), 0).unwrap();
        let closed = t.insert(Fd { cloexec: true, ..null_fd() }, 0).unwrap();
        t.close_on_exec();
        assert!(t.get(kept).is_ok());
        assert!(t.get(closed).is_err());
    }

    #[test]
    fn fd_table_clones_share_open_descriptions() {
        let mut parent = FdTable::with_stdio();
        let fd = parent.insert(null_fd(), 0).unwrap();
        let mut child = parent.clone();
        assert!(Arc::ptr_eq(&parent.get(fd).unwrap().open, &child.get(fd).unwrap().open));
        child.close(fd).unwrap();
        assert!(parent.get(fd).is_ok(), "closing in the child doesn't affect the parent");
    }

    #[test]
    fn stat_layout() {
        let path = std::env::temp_dir().join(format!("wasm-env-stat-{}", std::process::id()));
        std::fs::write(&path, b"hello").unwrap();
        let st = stat_bytes(&std::fs::metadata(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        let u32_at = |o: usize| u32::from_le_bytes(st[o..o + 4].try_into().unwrap());
        let i64_at = |o: usize| i64::from_le_bytes(st[o..o + 8].try_into().unwrap());
        assert_eq!(u32_at(16) & 0o170000, 0o100000, "st_mode: regular file");
        assert_eq!((u32_at(24), u32_at(28)), (0, 0), "owned by root");
        assert_eq!(i64_at(48), 5, "st_size");
        assert!(i64_at(88) > 1_600_000_000, "st_mtime");
        let dev = device_stat_bytes(Device::Null);
        assert_eq!(u32::from_le_bytes(dev[16..20].try_into().unwrap()) & 0o170000, 0o020000, "character device");
        assert_eq!(u64::from_le_bytes(dev[32..40].try_into().unwrap()), 1 << 8 | 3, "rdev 1:3");
    }

    #[test]
    fn resolve_edge_cases() {
        let root = std::env::temp_dir().join(format!("wasm-env-fs-edge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(root.join("file"), b"").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("a", root.join("b")).unwrap();
            std::os::unix::fs::symlink("b", root.join("a")).unwrap();
            std::os::unix::fs::symlink("../../../file", root.join("dir/up")).unwrap();
            std::os::unix::fs::symlink("dir", root.join("rel")).unwrap();
        }
        let fs = Fs::new(root.clone());
        assert_eq!(fs.resolve("/", "file/x", true), Err(ENOTDIR), "a file isn't a directory");
        assert_eq!(fs.resolve("/", "", true), Err(ENOENT));
        #[cfg(unix)]
        {
            assert_eq!(fs.resolve("/", "/a", true), Err(ELOOP), "symlink cycle");
            assert_eq!(fs.resolve("/", "/dir/up", true).unwrap(), "/file", "relative link can't climb out");
            assert_eq!(fs.resolve("/", "/rel/../file", true).unwrap(), "/file");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_dir_lists_dot_entries() {
        let root = std::env::temp_dir().join(format!("wasm-env-fs-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d/f"), b"").unwrap();
        let fs = Fs::new(root.clone());
        let mut names: Vec<_> = read_dir(&fs, "/d").unwrap().into_iter().map(|e| (e.name, e.dtype)).collect();
        names.sort();
        assert_eq!(names, [(".".into(), 4), ("..".into(), 4), ("f".into(), 8)]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_stays_inside_root() {
        let root = std::env::temp_dir().join(format!("wasm-env-fs-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/busybox"), b"").unwrap();
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(root.join("bin/sh"));
            std::os::unix::fs::symlink("/bin/busybox", root.join("bin/sh")).unwrap();
        }
        let fs = Fs::new(root.clone());

        assert_eq!(fs.resolve("/", "../../..", true).unwrap(), "/");
        assert_eq!(fs.resolve("/bin", "../bin/./busybox", true).unwrap(), "/bin/busybox");
        assert_eq!(fs.resolve("/", "/missing", true).unwrap(), "/missing");
        assert_eq!(fs.resolve("/", "/missing/x", true), Err(ENOENT));
        assert_eq!(fs.resolve("/", "/bin/new/", false).unwrap(), "/bin/new");
        assert_eq!(fs.device("/", "/dev/null"), Some(DevPath::Device(Device::Null)));
        assert_eq!(fs.device("/dev", "fd/3"), Some(DevPath::Fd(3)));
        assert_eq!(fs.device("/tmp", "../dev/../dev/zero"), Some(DevPath::Device(Device::Zero)));
        assert_eq!(fs.device("/", "/dev/sda"), None);
        assert_eq!(fs.device("/", "/proc/cpuinfo"), Some(DevPath::Synthetic(Synthetic::CpuInfo)));
        assert_eq!(fs.device("/sys", "devices/system/cpu/online"), Some(DevPath::Synthetic(Synthetic::CpuList)));
        assert_eq!(fs.device("/", "/proc/meminfo"), None);
        #[cfg(unix)]
        {
            assert_eq!(fs.resolve("/", "/bin/sh", true).unwrap(), "/bin/busybox");
            assert_eq!(fs.resolve("/", "/bin/sh", false).unwrap(), "/bin/sh");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
