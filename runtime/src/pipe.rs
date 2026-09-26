//! Pipes between guest processes. Guest processes are threads in one host
//! process, so a pipe is a shared buffer with a condition variable.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::abi::errno::*;
use crate::memory::SysResult;

/// Linux's default pipe capacity.
const CAPACITY: usize = 65536;

/// How often a blocked reader or writer checks for signals.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

#[derive(Default)]
struct State {
    data: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

#[derive(Default)]
pub struct Pipe {
    state: Mutex<State>,
    changed: Condvar,
}

/// One end of a pipe. Pipe ends are held by open file descriptions
/// (shared by dup and vfork), so dropping the last reference closes the
/// end: readers then see EOF, writers get EPIPE.
pub struct PipeEnd {
    pub pipe: Arc<Pipe>,
    pub write: bool,
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        let mut s = self.pipe.state.lock().unwrap();
        if self.write {
            s.writers -= 1;
        } else {
            s.readers -= 1;
        }
        self.pipe.changed.notify_all();
    }
}

/// Creates a pipe: (read end, write end).
pub fn new() -> (PipeEnd, PipeEnd) {
    let pipe = Arc::new(Pipe::default());
    {
        let mut s = pipe.state.lock().unwrap();
        s.readers = 1;
        s.writers = 1;
    }
    (PipeEnd { pipe: pipe.clone(), write: false }, PipeEnd { pipe, write: true })
}

impl Pipe {
    /// Blocks until data is available or every writer has closed (EOF).
    /// `interrupted` is polled while waiting; if it returns true, EINTR.
    pub fn read(&self, buf: &mut [u8], nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<usize> {
        let mut s = self.state.lock().unwrap();
        while s.data.is_empty() {
            if s.writers == 0 || buf.is_empty() {
                return Ok(0);
            }
            if nonblock {
                return Err(EAGAIN);
            }
            if interrupted() {
                return Err(EINTR);
            }
            s = self.changed.wait_timeout(s, SIGNAL_POLL).unwrap().0;
        }
        let n = buf.len().min(s.data.len());
        for (dst, src) in buf.iter_mut().zip(s.data.drain(..n)) {
            *dst = src;
        }
        self.changed.notify_all();
        Ok(n)
    }

    /// Writes all of `buf`, blocking while the pipe is full. EPIPE if no
    /// reader is left (the caller raises SIGPIPE).
    pub fn write(&self, buf: &[u8], nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<usize> {
        let mut s = self.state.lock().unwrap();
        let mut written = 0;
        while written < buf.len() {
            if s.readers == 0 {
                return if written > 0 { Ok(written) } else { Err(EPIPE) };
            }
            let room = CAPACITY.saturating_sub(s.data.len());
            if room == 0 {
                if nonblock {
                    return if written > 0 { Ok(written) } else { Err(EAGAIN) };
                }
                if interrupted() {
                    return if written > 0 { Ok(written) } else { Err(EINTR) };
                }
                s = self.changed.wait_timeout(s, SIGNAL_POLL).unwrap().0;
                continue;
            }
            let n = room.min(buf.len() - written);
            s.data.extend(&buf[written..written + n]);
            written += n;
            self.changed.notify_all();
        }
        Ok(written)
    }

    /// Bytes waiting to be read (FIONREAD).
    pub fn available(&self) -> usize {
        self.state.lock().unwrap().data.len()
    }

    /// For poll: whether a read wouldn't block (data, or EOF), whether a
    /// write wouldn't (room, or no reader: EPIPE), and whether the other
    /// side has gone (POLLHUP for readers, POLLERR for writers).
    pub fn readiness(&self) -> (bool, bool, bool, bool) {
        let s = self.state.lock().unwrap();
        let readable = !s.data.is_empty() || s.writers == 0;
        let writable = s.data.len() < CAPACITY || s.readers == 0;
        (readable, writable, s.writers == 0, s.readers == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eof_after_writer_closes() {
        let (r, w) = new();
        let never = || false;
        assert_eq!(w.pipe.write(b"hi", false, &never), Ok(2));
        drop(w);
        let mut buf = [0u8; 8];
        assert_eq!(r.pipe.read(&mut buf, false, &never), Ok(2));
        assert_eq!(&buf[..2], b"hi");
        assert_eq!(r.pipe.read(&mut buf, false, &never), Ok(0), "EOF");
    }

    #[test]
    fn epipe_after_reader_closes() {
        let (r, w) = new();
        drop(r);
        assert_eq!(w.pipe.write(b"x", false, &|| false), Err(EPIPE));
    }

    #[test]
    fn blocking_across_threads() {
        let (r, w) = new();
        let writer = std::thread::spawn(move || {
            let big = vec![7u8; CAPACITY * 3];
            w.pipe.write(&big, false, &|| false)
        });
        let mut total = 0;
        let mut buf = vec![0u8; 10000];
        loop {
            match r.pipe.read(&mut buf, false, &|| false).unwrap() {
                0 => break,
                n => total += n,
            }
        }
        assert_eq!(writer.join().unwrap(), Ok(CAPACITY * 3));
        assert_eq!(total, CAPACITY * 3);
    }
}
