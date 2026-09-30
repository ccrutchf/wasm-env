//! inotify on every host (docs/abi.md, "System calls"): a runtime object
//! whose watches are the `notify` crate's (inotify on Linux, FSEvents on
//! macOS, ReadDirectoryChangesW on Windows), and whose events it reads out
//! as Linux's `struct inotify_event`s: wd, mask, cookie, name length, and
//! the name (relative to a watched directory; none for the watched file
//! itself), NUL-padded to 4 bytes.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use notify::event::{AccessKind, AccessMode, CreateKind, EventKind, ModifyKind, RemoveKind, RenameMode};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::abi::errno::*;
use crate::memory::SysResult;

pub const IN_ACCESS: u32 = 0x1;
pub const IN_MODIFY: u32 = 0x2;
pub const IN_ATTRIB: u32 = 0x4;
pub const IN_CLOSE_WRITE: u32 = 0x8;
pub const IN_CLOSE_NOWRITE: u32 = 0x10;
pub const IN_OPEN: u32 = 0x20;
pub const IN_MOVED_FROM: u32 = 0x40;
pub const IN_MOVED_TO: u32 = 0x80;
pub const IN_CREATE: u32 = 0x100;
pub const IN_DELETE: u32 = 0x200;
pub const IN_DELETE_SELF: u32 = 0x400;
pub const IN_IGNORED: u32 = 0x8000;
pub const IN_ISDIR: u32 = 0x4000_0000;
/// add_watch: add to an existing watch's mask rather than replacing it.
pub const IN_MASK_ADD: u32 = 0x2000_0000;

/// How often a blocked reader checks for signals.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

struct Watch {
    host: PathBuf,
    mask: u32,
}

#[derive(Default)]
struct State {
    watches: HashMap<i32, Watch>,
    next: i32,
    /// Encoded events, oldest first.
    queue: VecDeque<Vec<u8>>,
    cookie: u32,
}

pub struct Inotify {
    state: Mutex<State>,
    changed: Condvar,
    watcher: Mutex<Option<RecommendedWatcher>>,
}

impl Inotify {
    pub fn new() -> SysResult<Arc<Inotify>> {
        let this = Arc::new(Inotify { state: Mutex::new(State::default()), changed: Condvar::new(), watcher: Mutex::new(None) });
        let weak: Weak<Inotify> = Arc::downgrade(&this);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let (Ok(event), Some(this)) = (event, weak.upgrade()) {
                this.deliver(event);
            }
        })
        .map_err(|_| ENOSYS)?;
        *this.watcher.lock().unwrap() = Some(watcher);
        Ok(this)
    }

    /// inotify_add_watch on a host path: its watch descriptor (the same
    /// one again for a path watched already).
    pub fn add_watch(&self, host: &Path, mask: u32) -> SysResult<i32> {
        if !host.exists() {
            return Err(ENOENT);
        }
        let mut st = self.state.lock().unwrap();
        if let Some((&wd, w)) = st.watches.iter_mut().find(|(_, w)| w.host == host) {
            w.mask = if mask & IN_MASK_ADD != 0 { w.mask | mask } else { mask };
            return Ok(wd);
        }
        let mut watcher = self.watcher.lock().unwrap();
        watcher.as_mut().ok_or(EBADF)?.watch(host, RecursiveMode::NonRecursive).map_err(|_| EACCES)?;
        st.next += 1;
        let wd = st.next;
        st.watches.insert(wd, Watch { host: host.to_path_buf(), mask });
        Ok(wd)
    }

    pub fn rm_watch(&self, wd: i32) -> SysResult<()> {
        let mut st = self.state.lock().unwrap();
        let w = st.watches.remove(&wd).ok_or(EINVAL)?;
        if let Some(watcher) = self.watcher.lock().unwrap().as_mut() {
            let _ = watcher.unwatch(&w.host);
        }
        st.queue.push_back(encode(wd, IN_IGNORED, 0, ""));
        self.changed.notify_all();
        Ok(())
    }

    /// A host event, queued for each watch it concerns.
    fn deliver(&self, e: notify::Event) {
        let mut st = self.state.lock().unwrap();
        let dir = |k: &CreateKind| if *k == CreateKind::Folder { IN_ISDIR } else { 0 };
        // (mask, path) pairs; a rename carries a cookie joining its halves.
        let mut events: Vec<(u32, &PathBuf)> = Vec::new();
        match &e.kind {
            EventKind::Create(k) => events.extend(e.paths.iter().map(|p| (IN_CREATE | dir(k), p))),
            EventKind::Remove(k) => {
                let isdir = if *k == RemoveKind::Folder { IN_ISDIR } else { 0 };
                events.extend(e.paths.iter().map(|p| (IN_DELETE | isdir, p)));
            }
            EventKind::Modify(ModifyKind::Metadata(_)) => events.extend(e.paths.iter().map(|p| (IN_ATTRIB, p))),
            EventKind::Modify(ModifyKind::Name(mode)) => match mode {
                RenameMode::From => events.extend(e.paths.iter().map(|p| (IN_MOVED_FROM, p))),
                RenameMode::To => events.extend(e.paths.iter().map(|p| (IN_MOVED_TO, p))),
                RenameMode::Both if e.paths.len() == 2 => events.extend([(IN_MOVED_FROM, &e.paths[0]), (IN_MOVED_TO, &e.paths[1])]),
                // (FSEvents doesn't say which half: whether it's there now does.)
                _ => events.extend(e.paths.iter().map(|p| (if p.exists() { IN_MOVED_TO } else { IN_MOVED_FROM }, p))),
            },
            EventKind::Modify(_) => events.extend(e.paths.iter().map(|p| (IN_MODIFY, p))),
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => events.extend(e.paths.iter().map(|p| (IN_CLOSE_WRITE, p))),
            EventKind::Access(AccessKind::Close(_)) => events.extend(e.paths.iter().map(|p| (IN_CLOSE_NOWRITE, p))),
            EventKind::Access(AccessKind::Open(_)) => events.extend(e.paths.iter().map(|p| (IN_OPEN, p))),
            EventKind::Access(_) => events.extend(e.paths.iter().map(|p| (IN_ACCESS, p))),
            _ => {}
        }
        let renames = events.iter().any(|(m, _)| m & (IN_MOVED_FROM | IN_MOVED_TO) != 0);
        let cookie = if renames {
            st.cookie += 1;
            st.cookie
        } else {
            0
        };
        let mut out = Vec::new();
        for (mask, path) in events {
            for (&wd, w) in &st.watches {
                // The watched path itself, or an entry of a watched directory.
                let (mask, name) = if *path == w.host {
                    let mask = if mask & IN_DELETE != 0 { IN_DELETE_SELF } else { mask };
                    (mask, String::new())
                } else if path.parent() == Some(w.host.as_path()) {
                    (mask, path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                } else {
                    continue;
                };
                if w.mask & mask & !IN_ISDIR != 0 {
                    out.push(encode(wd, mask, if mask & (IN_MOVED_FROM | IN_MOVED_TO) != 0 { cookie } else { 0 }, &name));
                }
            }
        }
        if !out.is_empty() {
            st.queue.extend(out);
            self.changed.notify_all();
        }
    }

    /// read(): as many whole events as fit (EINVAL if not even one does).
    pub fn read(&self, buf: &mut [u8], nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<i64> {
        let mut st = self.state.lock().unwrap();
        while st.queue.is_empty() {
            if nonblock {
                return Err(EAGAIN);
            }
            if interrupted() {
                return Err(EINTR);
            }
            st = self.changed.wait_timeout(st, SIGNAL_POLL).unwrap().0;
        }
        let mut n = 0;
        while let Some(e) = st.queue.front() {
            if n + e.len() > buf.len() {
                break;
            }
            buf[n..n + e.len()].copy_from_slice(e);
            n += e.len();
            st.queue.pop_front();
        }
        if n == 0 { Err(EINVAL) } else { Ok(n as i64) }
    }

    /// For poll: whether events are waiting.
    pub fn readable(&self) -> bool {
        !self.state.lock().unwrap().queue.is_empty()
    }

    /// Bytes waiting (FIONREAD).
    pub fn available(&self) -> usize {
        self.state.lock().unwrap().queue.iter().map(Vec::len).sum()
    }
}

/// A `struct inotify_event` with its name.
fn encode(wd: i32, mask: u32, cookie: u32, name: &str) -> Vec<u8> {
    let len = if name.is_empty() { 0 } else { (name.len() + 1).next_multiple_of(4) };
    let mut e = Vec::with_capacity(16 + len);
    e.extend_from_slice(&wd.to_le_bytes());
    e.extend_from_slice(&mask.to_le_bytes());
    e.extend_from_slice(&cookie.to_le_bytes());
    e.extend_from_slice(&(len as u32).to_le_bytes());
    e.extend_from_slice(name.as_bytes());
    e.resize(16 + len, 0);
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_come_as_linux_writes_them() {
        let e = encode(3, IN_CREATE, 0, "abc");
        assert_eq!(e.len(), 16 + 4);
        assert_eq!(&e[12..16], &4u32.to_le_bytes());
        assert_eq!(&e[16..], b"abc\0");
        assert_eq!(encode(1, IN_IGNORED, 0, "").len(), 16);
    }

    #[test]
    fn a_watched_directory_reports_its_entries() {
        let dir = std::env::temp_dir().join(format!("wasm-env-inotify-{}", crate::translate::unique()));
        std::fs::create_dir_all(&dir).unwrap();
        let i = Inotify::new().unwrap();
        let wd = i.add_watch(&dir, IN_CREATE | IN_DELETE).unwrap();
        std::fs::write(dir.join("f"), b"x").unwrap();
        std::fs::remove_file(dir.join("f")).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while got.len() < 2 && std::time::Instant::now() < deadline {
            if let Ok(n) = i.read(&mut buf, true, &|| false) {
                let mut at = 0;
                while at < n as usize {
                    let mask = u32::from_le_bytes(buf[at + 4..at + 8].try_into().unwrap());
                    let len = u32::from_le_bytes(buf[at + 12..at + 16].try_into().unwrap()) as usize;
                    let name = String::from_utf8_lossy(&buf[at + 16..at + 16 + len]).trim_end_matches('\0').to_string();
                    assert_eq!(i32::from_le_bytes(buf[at..at + 4].try_into().unwrap()), wd);
                    got.push((mask, name));
                    at += 16 + len;
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert_eq!(got, [(IN_CREATE, "f".to_string()), (IN_DELETE, "f".to_string())]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
