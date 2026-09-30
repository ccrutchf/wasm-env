//! File locks, the runtime's own on every host: fcntl's record locks (a
//! process's, and open file descriptions' F_OFD_ ones) and flock's
//! whole-file locks. The environment's processes share one host process, so
//! the host's per-process locks could never make them wait for each other;
//! the runtime keeps the table instead, as Linux's kernel does, keyed by
//! the host file's identity. Locks are between the environment's processes:
//! host programs outside it don't see them.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, Weak};
use std::time::Duration;

use crate::abi::errno::*;
use crate::fs::Open;
use crate::memory::SysResult;

pub const F_RDLCK: i16 = 0;
pub const F_WRLCK: i16 = 1;
pub const F_UNLCK: i16 = 2;

/// How often a blocked lock checks for signals.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

/// A host file's identity: (device, inode) on Unix hosts, (volume serial,
/// file index) on Windows.
pub type FileId = (u64, u64);

pub fn file_id(file: &std::fs::File) -> SysResult<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata().map_err(|e| crate::fs::errno(&e))?;
        Ok((m.dev(), m.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
        // SAFETY: an open handle and a struct to fill.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(EIO);
        }
        Ok((info.dwVolumeSerialNumber as u64, (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64))
    }
}

/// Who holds a lock.
#[derive(Clone)]
pub enum Owner {
    /// A process (classic POSIX record locks): released when it closes any
    /// descriptor for the file, or exits.
    Process(i32),
    /// An open file description (F_OFD_ locks, flock): released when the
    /// last descriptor for it closes.
    Description(Weak<Mutex<Open>>),
}

impl Owner {
    fn same(&self, other: &Owner) -> bool {
        match (self, other) {
            (Owner::Process(a), Owner::Process(b)) => a == b,
            (Owner::Description(a), Owner::Description(b)) => Weak::ptr_eq(a, b),
            _ => false,
        }
    }

    fn gone(&self) -> bool {
        matches!(self, Owner::Description(d) if d.strong_count() == 0)
    }
}

#[derive(Clone)]
struct Lock {
    owner: Owner,
    /// Bytes [start, end); `end` u64::MAX is "to the end of the file, however
    /// long it grows".
    start: u64,
    end: u64,
    write: bool,
}

/// A conflicting lock, as F_GETLK reports it.
#[derive(Debug, PartialEq)]
pub struct Held {
    pub write: bool,
    pub start: u64,
    /// 0: to the end of the file.
    pub len: u64,
    /// The holding process; -1 for an open file description's lock.
    pub pid: i32,
}

/// Record locks and flock locks are separate on Linux: neither sees the
/// other's.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Record,
    Flock,
}

#[derive(Default)]
pub struct Locks {
    files: Mutex<HashMap<(Kind, FileId), Vec<Lock>>>,
    changed: Condvar,
}

impl Locks {
    /// The first lock another owner holds that would conflict with this one.
    pub fn test(&self, kind: Kind, file: FileId, owner: &Owner, start: u64, end: u64, write: bool) -> Option<Held> {
        let mut files = self.files.lock().unwrap();
        let locks = files.entry((kind, file)).or_default();
        locks.retain(|l| !l.owner.gone());
        conflict(locks, owner, start, end, write).map(|l| Held {
            write: l.write,
            start: l.start,
            len: if l.end == u64::MAX { 0 } else { l.end - l.start },
            pid: match l.owner {
                Owner::Process(pid) => pid,
                Owner::Description(_) => -1,
            },
        })
    }

    /// Takes (or with `ty` F_UNLCK, releases) [start, end) for `owner`,
    /// replacing what it held there: at once or EAGAIN, or with `wait`
    /// once no other owner's lock conflicts (EINTR if interrupted first).
    #[allow(clippy::too_many_arguments)]
    pub fn set(&self, kind: Kind, file: FileId, owner: Owner, start: u64, end: u64, ty: i16, wait: bool, interrupted: &dyn Fn() -> bool) -> SysResult<()> {
        if start >= end {
            return Err(EINVAL);
        }
        let mut files = self.files.lock().unwrap();
        loop {
            let locks = files.entry((kind, file)).or_default();
            locks.retain(|l| !l.owner.gone());
            if ty == F_UNLCK || conflict(locks, &owner, start, end, ty == F_WRLCK).is_none() {
                break;
            }
            if !wait {
                return Err(EAGAIN);
            }
            if interrupted() {
                return Err(EINTR);
            }
            files = self.changed.wait_timeout(files, SIGNAL_POLL).unwrap().0;
        }
        let locks = files.get_mut(&(kind, file)).unwrap();
        // What the owner held in [start, end) goes; what it held around it stays.
        let mut kept = Vec::with_capacity(locks.len() + 2);
        for l in locks.drain(..) {
            if !l.owner.same(&owner) || l.end <= start || l.start >= end {
                kept.push(l);
                continue;
            }
            if l.start < start {
                kept.push(Lock { end: start, ..l.clone() });
            }
            if l.end > end {
                kept.push(Lock { start: end, ..l });
            }
        }
        if ty != F_UNLCK {
            kept.push(Lock { owner, start, end, write: ty == F_WRLCK });
        }
        *locks = kept;
        if locks.is_empty() {
            files.remove(&(kind, file));
        }
        self.changed.notify_all();
        Ok(())
    }

    /// A process closed a descriptor for `file`: its record locks there go.
    pub fn closed(&self, pid: i32, file: FileId) {
        let mut files = self.files.lock().unwrap();
        if let Some(locks) = files.get_mut(&(Kind::Record, file)) {
            locks.retain(|l| !matches!(l.owner, Owner::Process(p) if p == pid));
            if locks.is_empty() {
                files.remove(&(Kind::Record, file));
            }
            self.changed.notify_all();
        }
    }

    /// A process ended: all its record locks go (and open file
    /// descriptions it held the last descriptor for are gone too).
    pub fn exited(&self, pid: i32) {
        let mut files = self.files.lock().unwrap();
        files.retain(|_, locks| {
            locks.retain(|l| !l.owner.gone() && !matches!(l.owner, Owner::Process(p) if p == pid));
            !locks.is_empty()
        });
        self.changed.notify_all();
    }

    /// A descriptor closed somewhere: waiters recheck (an open file
    /// description's locks go with its last descriptor).
    pub fn released(&self) {
        let _files = self.files.lock().unwrap();
        self.changed.notify_all();
    }
}

fn conflict<'a>(locks: &'a [Lock], owner: &Owner, start: u64, end: u64, write: bool) -> Option<&'a Lock> {
    locks.iter().find(|l| !l.owner.same(owner) && l.start < end && start < l.end && (write || l.write))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const F: FileId = (1, 2);
    fn never() -> bool {
        false
    }

    #[test]
    fn readers_share_and_writers_exclude() {
        let l = Locks::default();
        let (a, b) = (Owner::Process(1), Owner::Process(2));
        l.set(Kind::Record, F, a.clone(), 0, 10, F_RDLCK, false, &never).unwrap();
        l.set(Kind::Record, F, b.clone(), 5, 15, F_RDLCK, false, &never).unwrap();
        assert_eq!(l.set(Kind::Record, F, b.clone(), 0, 1, F_WRLCK, false, &never), Err(EAGAIN));
        assert_eq!(l.test(Kind::Record, F, &b, 0, 1, true), Some(Held { write: false, start: 0, len: 10, pid: 1 }));
        // Beyond a's range, b may write; flock locks don't see record locks.
        l.set(Kind::Record, F, b.clone(), 10, u64::MAX, F_WRLCK, false, &never).unwrap();
        l.set(Kind::Flock, F, a.clone(), 0, u64::MAX, F_WRLCK, false, &never).unwrap();
    }

    #[test]
    fn unlocking_the_middle_splits_a_lock() {
        let l = Locks::default();
        let (a, b) = (Owner::Process(1), Owner::Process(2));
        l.set(Kind::Record, F, a.clone(), 0, 30, F_WRLCK, false, &never).unwrap();
        l.set(Kind::Record, F, a.clone(), 10, 20, F_UNLCK, false, &never).unwrap();
        assert!(l.test(Kind::Record, F, &b, 10, 20, true).is_none());
        assert_eq!(l.test(Kind::Record, F, &b, 25, 26, false).map(|h| (h.start, h.len)), Some((20, 10)));
        l.closed(1, F);
        assert!(l.test(Kind::Record, F, &b, 0, u64::MAX, true).is_none());
    }

    #[test]
    fn a_description_lock_goes_with_the_description() {
        let l = Locks::default();
        let open = Arc::new(Mutex::new(Open::Stdin));
        l.set(Kind::Flock, F, Owner::Description(Arc::downgrade(&open)), 0, u64::MAX, F_WRLCK, false, &never).unwrap();
        assert_eq!(l.set(Kind::Flock, F, Owner::Process(9), 0, u64::MAX, F_RDLCK, false, &never), Err(EAGAIN));
        drop(open);
        l.set(Kind::Flock, F, Owner::Process(9), 0, u64::MAX, F_RDLCK, false, &never).unwrap();
    }

    #[test]
    fn a_file_is_the_same_file_however_opened() {
        let dir = std::env::temp_dir().join(format!("wasm-env-locks-{}", crate::translate::unique()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), b"a").unwrap();
        std::fs::write(dir.join("b"), b"b").unwrap();
        let open = |n| std::fs::File::open(dir.join(n)).unwrap();
        assert_eq!(file_id(&open("a")).unwrap(), file_id(&open("a")).unwrap());
        assert_ne!(file_id(&open("a")).unwrap(), file_id(&open("b")).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_waiter_gets_the_lock_when_it_is_released() {
        let l = Arc::new(Locks::default());
        l.set(Kind::Record, F, Owner::Process(1), 0, 1, F_WRLCK, false, &never).unwrap();
        let l2 = l.clone();
        let t = std::thread::spawn(move || l2.set(Kind::Record, F, Owner::Process(2), 0, 1, F_WRLCK, true, &never));
        std::thread::sleep(Duration::from_millis(20));
        l.exited(1);
        t.join().unwrap().unwrap();
    }
}
