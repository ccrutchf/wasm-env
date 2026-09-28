//! eventfd and epoll, the way libuv (Node.js) and other event loops wait:
//! an eventfd is a counter that `write` adds to and `read` takes, readable
//! while it isn't zero; an epoll instance is an interest list of
//! descriptors, waited on together (by way of `poll_fds`, which knows every
//! kind of descriptor's readiness). Readiness is level-triggered: EPOLLET is
//! accepted and behaves like it, which suits event loops that read until
//! EAGAIN; EPOLLONESHOT disables a descriptor once it's reported.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::abi::errno::*;
use crate::memory::SysResult;

/// An eventfd (eventfd2).
pub struct EventFd {
    count: Mutex<u64>,
    changed: Condvar,
    /// EFD_SEMAPHORE: `read` takes 1 at a time.
    semaphore: bool,
}

const MAX: u64 = u64::MAX - 1;

impl EventFd {
    pub fn new(initial: u64, semaphore: bool) -> EventFd {
        EventFd { count: Mutex::new(initial), changed: Condvar::new(), semaphore }
    }

    /// (readable, writable).
    pub fn readiness(&self) -> (bool, bool) {
        let n = *self.count.lock().unwrap();
        (n > 0, n < MAX)
    }

    /// read(2): the 8-byte counter (or 1, as a semaphore), which it takes;
    /// blocks while it's zero, unless `nonblock`.
    pub fn read(&self, buf: &mut [u8], nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<i64> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        let mut n = self.count.lock().unwrap();
        while *n == 0 {
            if nonblock {
                return Err(EAGAIN);
            }
            if interrupted() {
                return Err(EINTR);
            }
            n = self.changed.wait_timeout(n, Duration::from_millis(20)).unwrap().0;
        }
        let taken = if self.semaphore { 1 } else { *n };
        *n -= taken;
        buf[..8].copy_from_slice(&taken.to_le_bytes());
        self.changed.notify_all();
        Ok(8)
    }

    /// write(2): adds the 8-byte value; blocks while that would overflow,
    /// unless `nonblock`.
    pub fn write(&self, buf: &[u8], nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<i64> {
        let Some(v) = buf.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap())) else { return Err(EINVAL) };
        if v == u64::MAX {
            return Err(EINVAL);
        }
        let mut n = self.count.lock().unwrap();
        while *n > MAX - v {
            if nonblock {
                return Err(EAGAIN);
            }
            if interrupted() {
                return Err(EINTR);
            }
            n = self.changed.wait_timeout(n, Duration::from_millis(20)).unwrap().0;
        }
        *n += v;
        self.changed.notify_all();
        Ok(8)
    }
}

/// An epoll instance: descriptor → (events, data).
#[derive(Default)]
pub struct Epoll {
    pub interest: Mutex<BTreeMap<i64, (u32, u64)>>,
}

pub const EPOLLIN: u32 = 0x1;
pub const EPOLLPRI: u32 = 0x2;
pub const EPOLLOUT: u32 = 0x4;
pub const EPOLLERR: u32 = 0x8;
pub const EPOLLHUP: u32 = 0x10;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLLONESHOT: u32 = 1 << 30;

impl Epoll {
    /// epoll_ctl(ADD, MOD or DEL).
    pub fn ctl(&self, op: i64, fd: i64, events: u32, data: u64) -> SysResult<()> {
        const ADD: i64 = 1;
        const DEL: i64 = 2;
        const MOD: i64 = 3;
        let mut interest = self.interest.lock().unwrap();
        match op {
            ADD if interest.contains_key(&fd) => Err(EEXIST),
            ADD | MOD if op == MOD && !interest.contains_key(&fd) => Err(ENOENT),
            ADD | MOD => {
                interest.insert(fd, (events, data));
                Ok(())
            }
            DEL => interest.remove(&fd).map(|_| ()).ok_or(ENOENT),
            _ => Err(EINVAL),
        }
    }
}

/// struct epoll_event on arm64: u32 events, 4 bytes of padding, u64 data.
pub const EVENT_SIZE: u64 = 16;
