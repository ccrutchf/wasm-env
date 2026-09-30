//! Unix sockets (docs/abi.md, "Networking"): runtime objects, like pipes,
//! on every host. Guest processes are threads of one host process, so a
//! socket is an inbox of segments with a condition variable; a connection
//! is two sockets that write into each other's inboxes. Names (paths under
//! the root, abstract names) live in the kernel's namespace, where a name
//! leads to a listening guest socket or to a server of the runtime's own
//! (its Wayland compositor). Descriptors passed with SCM_RIGHTS are the
//! open file descriptions themselves.
//!
//! (A path forwarded to a host socket, the host's compositor with
//! `--wayland=host`, is connected on the host instead: see net.rs.)

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use crate::abi::errno::*;
use crate::fs::Open;
use crate::memory::SysResult;

pub const SOCK_STREAM: i64 = 1;
pub const SOCK_DGRAM: i64 = 2;
pub const SOCK_SEQPACKET: i64 = 5;

const ECONNREFUSED: i64 = 111;
const EISCONN: i64 = 106;
const ENOTCONN: i64 = 107;
const EADDRINUSE: i64 = 98;
const EOPNOTSUPP: i64 = 95;
const EPROTOTYPE: i64 = 91;
const EMSGSIZE: i64 = 90;
const EDESTADDRREQ: i64 = 89;

/// How much an inbox holds before senders wait (Linux's default socket
/// buffers are about this size).
const CAPACITY: usize = 212_992;

/// How often a blocked caller checks for signals.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

/// An open file description, as descriptors share it.
pub type Description = Arc<Mutex<Open>>;

/// A descriptor passed in an SCM_RIGHTS message: its description and its
/// file status flags.
pub struct Passed {
    pub open: Description,
    pub flags: i64,
}

impl std::fmt::Debug for Passed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Passed(flags {:#o})", self.flags)
    }
}

/// A socket's name: a path (the guest's, resolved) or an abstract name.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Name {
    Path(String),
    Abstract(Vec<u8>),
}

/// Bytes sent together, with the descriptors sent with them and, for
/// datagrams, the sender's name.
struct Segment {
    data: Vec<u8>,
    pos: usize,
    fds: Vec<Passed>,
    from: Option<Vec<u8>>,
}

enum Role {
    Idle,
    Listening { queue: VecDeque<Arc<UnixSocket>>, backlog: usize },
    /// Connected (a datagram socket: to its default destination).
    Connected { peer: Weak<UnixSocket>, peer_pid: i32 },
}

struct Inner {
    name: Option<Name>,
    /// The address as the guest gave it (a relative path stays relative):
    /// what getsockname reports, and a datagram's sender.
    shown: Option<Vec<u8>>,
    role: Role,
    inbox: VecDeque<Segment>,
    /// Bytes waiting in the inbox.
    queued: usize,
    /// The peer won't send more (it closed, or shut down writing).
    eof: bool,
    shut_read: bool,
    shut_write: bool,
}

pub struct UnixSocket {
    pub kind: i64,
    inner: Mutex<Inner>,
    changed: Condvar,
}

/// What a receive returned: bytes, descriptors, the sender's name
/// (datagrams), and whether the message was longer than asked for.
pub struct Received {
    pub data: Vec<u8>,
    pub fds: Vec<Passed>,
    pub from: Option<Vec<u8>>,
    pub truncated: bool,
    /// A datagram's whole length (MSG_TRUNC reports it).
    pub full: usize,
}

/// Where a name leads.
pub enum Listener {
    Socket(Weak<UnixSocket>),
    /// A server of the runtime's: it gets the server side of each
    /// connection made to the name.
    Server(Arc<dyn Fn(Arc<UnixSocket>) + Send + Sync>),
}

/// The kernel's names (one namespace for all processes).
#[derive(Default)]
pub struct Names {
    map: Mutex<HashMap<Name, Listener>>,
}

impl Names {
    /// Makes `name` lead to a server of the runtime's.
    pub fn serve(&self, name: Name, server: Arc<dyn Fn(Arc<UnixSocket>) + Send + Sync>) {
        self.map.lock().unwrap().insert(name, Listener::Server(server));
    }

    /// Whether `path` names a socket (for stat).
    pub fn is_socket_path(&self, path: &str) -> bool {
        self.map.lock().unwrap().contains_key(&Name::Path(path.to_string()))
    }

    /// The name is gone (its file was unlinked).
    pub fn forget(&self, path: &str) {
        self.map.lock().unwrap().remove(&Name::Path(path.to_string()));
    }

    fn lookup(&self, name: &Name) -> Option<Listener> {
        match self.map.lock().unwrap().get(name)? {
            Listener::Socket(w) => Some(Listener::Socket(w.clone())),
            Listener::Server(f) => Some(Listener::Server(f.clone())),
        }
    }

    /// The bound datagram socket behind a name.
    fn socket(&self, name: &Name) -> SysResult<Arc<UnixSocket>> {
        match self.lookup(name) {
            Some(Listener::Socket(w)) => w.upgrade().ok_or(ECONNREFUSED),
            Some(Listener::Server(_)) => Err(EPROTOTYPE),
            None => Err(if matches!(name, Name::Path(_)) { ENOENT } else { ECONNREFUSED }),
        }
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        // The peer reads EOF, and writes EPIPE.
        let peer = match &self.inner.get_mut().unwrap().role {
            Role::Connected { peer, .. } => peer.upgrade(),
            _ => None,
        };
        if let Some(peer) = peer {
            peer.inner.lock().unwrap().eof = true;
            peer.changed.notify_all();
        }
    }
}

impl UnixSocket {
    pub fn new(kind: i64) -> Arc<UnixSocket> {
        Arc::new(UnixSocket {
            kind,
            inner: Mutex::new(Inner {
                name: None,
                shown: None,
                role: Role::Idle,
                inbox: VecDeque::new(),
                queued: 0,
                eof: false,
                shut_read: false,
                shut_write: false,
            }),
            changed: Condvar::new(),
        })
    }

    /// socketpair: two sockets connected to each other.
    pub fn pair(kind: i64, pid: i32) -> (Arc<UnixSocket>, Arc<UnixSocket>) {
        let (a, b) = (UnixSocket::new(kind), UnixSocket::new(kind));
        a.inner.lock().unwrap().role = Role::Connected { peer: Arc::downgrade(&b), peer_pid: pid };
        b.inner.lock().unwrap().role = Role::Connected { peer: Arc::downgrade(&a), peer_pid: pid };
        (a, b)
    }

    fn stream(&self) -> bool {
        self.kind == SOCK_STREAM
    }

    /// The address as bound (None: unnamed).
    pub fn address(&self) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().shown.clone()
    }

    /// The connected peer's address, and its process (SO_PEERCRED).
    pub fn peer(&self) -> SysResult<(Option<Vec<u8>>, i32)> {
        let (peer, pid) = match &self.inner.lock().unwrap().role {
            Role::Connected { peer, peer_pid } => (peer.clone(), *peer_pid),
            _ => return Err(ENOTCONN),
        };
        Ok((peer.upgrade().and_then(|p| p.address()), pid))
    }

    /// bind: the name leads to this socket. (For a path, the caller has
    /// made its file.)
    pub fn bind(self: &Arc<Self>, names: &Names, name: Name, shown: Vec<u8>) -> SysResult<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.name.is_some() {
            return Err(EINVAL);
        }
        let mut map = names.map.lock().unwrap();
        let taken = match map.get(&name) {
            Some(Listener::Socket(w)) => w.strong_count() > 0,
            Some(Listener::Server(_)) => true,
            None => false,
        };
        // (A path's file is the real guard: the caller made it anew.)
        if taken && matches!(name, Name::Abstract(_)) {
            return Err(EADDRINUSE);
        }
        map.insert(name.clone(), Listener::Socket(Arc::downgrade(self)));
        inner.name = Some(name);
        inner.shown = Some(shown);
        Ok(())
    }

    pub fn listen(&self, backlog: i64) -> SysResult<()> {
        if !matches!(self.kind, SOCK_STREAM | SOCK_SEQPACKET) {
            return Err(EOPNOTSUPP);
        }
        let mut inner = self.inner.lock().unwrap();
        match &mut inner.role {
            Role::Idle => {
                inner.role = Role::Listening { queue: VecDeque::new(), backlog: backlog.clamp(1, 4096) as usize };
                Ok(())
            }
            Role::Listening { backlog: b, .. } => {
                *b = backlog.clamp(1, 4096) as usize;
                Ok(())
            }
            Role::Connected { .. } => Err(EINVAL),
        }
    }

    /// connect: to a listening socket (a new connection it accepts), a
    /// server of the runtime's, or for a datagram socket, a default
    /// destination.
    pub fn connect(
        self: &Arc<Self>,
        names: &Names,
        name: &Name,
        pid: i32,
        nonblock: bool,
        interrupted: &dyn Fn() -> bool,
    ) -> SysResult<()> {
        if self.kind == SOCK_DGRAM {
            let target = names.socket(name)?;
            if target.kind != SOCK_DGRAM {
                return Err(EPROTOTYPE);
            }
            self.inner.lock().unwrap().role = Role::Connected { peer: Arc::downgrade(&target), peer_pid: pid };
            return Ok(());
        }
        if matches!(self.inner.lock().unwrap().role, Role::Connected { .. } | Role::Listening { .. }) {
            return Err(EISCONN);
        }
        match names.lookup(name) {
            None => Err(if matches!(name, Name::Path(_)) { ENOENT } else { ECONNREFUSED }),
            Some(Listener::Server(serve)) => {
                let server = UnixSocket::new(self.kind);
                server.inner.lock().unwrap().name = Some(name.clone());
                self.join(&server, pid);
                serve(server);
                Ok(())
            }
            Some(Listener::Socket(w)) => {
                let listener = w.upgrade().ok_or(ECONNREFUSED)?;
                if listener.kind != self.kind {
                    return Err(EPROTOTYPE);
                }
                let mut l = listener.inner.lock().unwrap();
                loop {
                    let (lname, lshown) = (l.name.clone(), l.shown.clone());
                    let Role::Listening { queue, backlog } = &mut l.role else { return Err(ECONNREFUSED) };
                    if queue.len() < *backlog {
                        let server = UnixSocket::new(self.kind);
                        {
                            let mut s = server.inner.lock().unwrap();
                            (s.name, s.shown) = (lname, lshown);
                        }
                        self.join(&server, pid);
                        queue.push_back(server);
                        listener.changed.notify_all();
                        return Ok(());
                    }
                    if nonblock {
                        return Err(EAGAIN);
                    }
                    if interrupted() {
                        return Err(EINTR);
                    }
                    l = listener.changed.wait_timeout(l, SIGNAL_POLL).unwrap().0;
                }
            }
        }
    }

    /// Connects this (client) socket and `server` to each other.
    fn join(self: &Arc<Self>, server: &Arc<UnixSocket>, pid: i32) {
        self.inner.lock().unwrap().role = Role::Connected { peer: Arc::downgrade(server), peer_pid: pid };
        server.inner.lock().unwrap().role = Role::Connected { peer: Arc::downgrade(self), peer_pid: pid };
    }

    /// accept: the next connection made to this listening socket.
    pub fn accept(&self, nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<Arc<UnixSocket>> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            let Role::Listening { queue, .. } = &mut inner.role else { return Err(EINVAL) };
            if let Some(s) = queue.pop_front() {
                self.changed.notify_all();
                return Ok(s);
            }
            if nonblock {
                return Err(EAGAIN);
            }
            if interrupted() {
                return Err(EINTR);
            }
            inner = self.changed.wait_timeout(inner, SIGNAL_POLL).unwrap().0;
        }
    }

    /// Sends `data` (with `fds`) to the connected peer, or for a datagram
    /// socket, to `to` if given. A stream sends what fits, waiting for
    /// room unless `nonblock`; messages go whole. EPIPE when nobody reads.
    pub fn send(
        &self,
        names: &Names,
        data: &[u8],
        fds: Vec<Passed>,
        to: Option<&Name>,
        nonblock: bool,
        interrupted: &dyn Fn() -> bool,
    ) -> SysResult<usize> {
        let (my_address, shut_write, peer) = {
            let inner = self.inner.lock().unwrap();
            let peer = match &inner.role {
                Role::Connected { peer, .. } => Some(peer.clone()),
                _ => None,
            };
            (inner.shown.clone(), inner.shut_write, peer)
        };
        if shut_write {
            return Err(EPIPE);
        }
        let target = match (to, self.kind) {
            (Some(name), SOCK_DGRAM) => names.socket(name)?,
            (Some(_), _) if peer.is_some() => return Err(EISCONN),
            _ => match peer {
                Some(p) => p.upgrade().ok_or(if self.kind == SOCK_DGRAM { ECONNREFUSED } else { EPIPE })?,
                None => return Err(if self.kind == SOCK_DGRAM { EDESTADDRREQ } else { ENOTCONN }),
            },
        };
        if !self.stream() && data.len() > CAPACITY {
            return Err(EMSGSIZE);
        }
        let from = if self.kind == SOCK_DGRAM { my_address } else { None };
        let mut fds = Some(fds);
        let mut sent = 0;
        let mut t = target.inner.lock().unwrap();
        loop {
            if t.shut_read {
                return if sent > 0 { Ok(sent) } else { Err(EPIPE) };
            }
            let room = CAPACITY.saturating_sub(t.queued);
            let fits = if self.stream() { room > 0 } else { data.len() <= room };
            if fits {
                let n = if self.stream() { room.min(data.len() - sent) } else { data.len() };
                t.inbox.push_back(Segment { data: data[sent..sent + n].to_vec(), pos: 0, fds: fds.take().unwrap_or_default(), from: from.clone() });
                t.queued += n;
                sent += n;
                target.changed.notify_all();
                if sent == data.len() {
                    return Ok(sent);
                }
                continue;
            }
            if nonblock {
                return if sent > 0 { Ok(sent) } else { Err(EAGAIN) };
            }
            if interrupted() {
                return if sent > 0 { Ok(sent) } else { Err(EINTR) };
            }
            t = target.changed.wait_timeout(t, SIGNAL_POLL).unwrap().0;
        }
    }

    /// Receives up to `len` bytes, and the descriptors sent with them (a
    /// stream read stops where descriptors begin or end, as Linux's does).
    /// `peek` leaves them queued; `waitall` waits for `len` bytes of a
    /// stream. Ok with no bytes at EOF.
    pub fn recv(&self, len: usize, peek: bool, waitall: bool, nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<Received> {
        let mut inner = self.inner.lock().unwrap();
        let mut got = Received { data: Vec::new(), fds: Vec::new(), from: None, truncated: false, full: 0 };
        loop {
            if inner.shut_read {
                return Ok(got);
            }
            if !inner.inbox.is_empty() {
                if !self.stream() {
                    // A whole message; its rest is dropped if it doesn't fit.
                    let seg = if peek { None } else { inner.inbox.pop_front() };
                    let (data, from, fds) = match &seg {
                        Some(s) => (&s.data, s.from.clone(), Vec::new()),
                        None => {
                            let s = inner.inbox.front().unwrap();
                            (&s.data, s.from.clone(), Vec::new())
                        }
                    };
                    let n = data.len().min(len);
                    got.data = data[..n].to_vec();
                    got.truncated = data.len() > len;
                    got.full = data.len();
                    got.from = from;
                    got.fds = fds;
                    if let Some(s) = seg {
                        inner.queued -= s.data.len();
                        got.fds = s.fds;
                        self.changed.notify_all();
                    }
                    return Ok(got);
                }
                // Stream: bytes from successive segments, not across one
                // that carries descriptors.
                let mut k = 0;
                while got.data.len() < len && k < inner.inbox.len() {
                    let seg = &inner.inbox[k];
                    if !seg.fds.is_empty() && !got.data.is_empty() {
                        break;
                    }
                    let n = (seg.data.len() - seg.pos).min(len - got.data.len());
                    got.data.extend_from_slice(&seg.data[seg.pos..seg.pos + n]);
                    let carries = !seg.fds.is_empty();
                    if !peek {
                        let seg = &mut inner.inbox[k];
                        seg.pos += n;
                        got.fds.append(&mut seg.fds);
                        inner.queued -= n;
                    }
                    k += 1;
                    if carries {
                        break;
                    }
                }
                if !peek {
                    while inner.inbox.front().is_some_and(|s| s.pos == s.data.len() && s.fds.is_empty()) {
                        inner.inbox.pop_front();
                    }
                    self.changed.notify_all();
                }
                if !(waitall && got.data.len() < len && !inner.eof && !peek) {
                    return Ok(got);
                }
            }
            if inner.eof || len == 0 {
                return Ok(got);
            }
            if matches!(inner.role, Role::Idle | Role::Listening { .. }) && self.kind != SOCK_DGRAM {
                return Err(ENOTCONN);
            }
            if nonblock {
                return if got.data.is_empty() { Err(EAGAIN) } else { Ok(got) };
            }
            if interrupted() {
                return if got.data.is_empty() { Err(EINTR) } else { Ok(got) };
            }
            inner = self.changed.wait_timeout(inner, SIGNAL_POLL).unwrap().0;
        }
    }

    /// shutdown: 0 reading, 1 writing, 2 both.
    pub fn shutdown(&self, how: i64) -> SysResult<()> {
        let peer = {
            let mut inner = self.inner.lock().unwrap();
            let Role::Connected { peer, .. } = &inner.role else { return Err(ENOTCONN) };
            let peer = peer.upgrade();
            if how == 0 || how == 2 {
                inner.shut_read = true;
            }
            if how == 1 || how == 2 {
                inner.shut_write = true;
            }
            self.changed.notify_all();
            peer
        };
        if let Some(p) = peer
            && (how == 1 || how == 2)
        {
            p.inner.lock().unwrap().eof = true;
            p.changed.notify_all();
        }
        Ok(())
    }

    /// Bytes waiting (FIONREAD).
    pub fn available(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        if self.stream() { inner.queued } else { inner.inbox.front().map_or(0, |s| s.data.len()) }
    }

    /// For poll: (readable, writable, hung up).
    pub fn readiness(&self) -> (bool, bool, bool) {
        let (peer, readable, eof) = {
            let inner = self.inner.lock().unwrap();
            let readable = match &inner.role {
                Role::Listening { queue, .. } => !queue.is_empty(),
                _ => !inner.inbox.is_empty() || inner.eof || inner.shut_read,
            };
            let peer = match &inner.role {
                Role::Connected { peer, .. } => Some(peer.clone()),
                _ => None,
            };
            (peer, readable, inner.eof)
        };
        let writable = match peer.map(|p| p.upgrade()) {
            Some(Some(p)) => p.inner.lock().unwrap().queued < CAPACITY,
            Some(None) => true, // a write fails at once (EPIPE)
            None => self.kind == SOCK_DGRAM,
        };
        (readable, writable, eof && self.kind != SOCK_DGRAM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> bool {
        false
    }

    #[test]
    fn a_pair_carries_bytes_and_eof() {
        let names = Names::default();
        let (a, b) = UnixSocket::pair(SOCK_STREAM, 1);
        assert_eq!(a.send(&names, b"hello", Vec::new(), None, false, &never), Ok(5));
        assert_eq!(b.recv(3, false, false, false, &never).unwrap().data, b"hel");
        assert_eq!(b.recv(10, false, false, false, &never).unwrap().data, b"lo");
        drop(a);
        assert!(b.recv(10, false, false, false, &never).unwrap().data.is_empty(), "EOF");
        assert_eq!(b.send(&names, b"x", Vec::new(), None, false, &never), Err(EPIPE));
    }

    #[test]
    fn a_stream_read_stops_at_descriptors() {
        let names = Names::default();
        let (a, b) = UnixSocket::pair(SOCK_STREAM, 1);
        a.send(&names, b"ab", Vec::new(), None, false, &never).unwrap();
        let fd = Passed { open: Arc::new(Mutex::new(Open::Stdin)), flags: 0 };
        a.send(&names, b"cd", vec![fd], None, false, &never).unwrap();
        a.send(&names, b"ef", Vec::new(), None, false, &never).unwrap();
        let first = b.recv(10, false, false, false, &never).unwrap();
        assert_eq!((first.data.as_slice(), first.fds.len()), (&b"ab"[..], 0));
        let second = b.recv(10, false, false, false, &never).unwrap();
        assert_eq!((second.data.as_slice(), second.fds.len()), (&b"cd"[..], 1));
        assert_eq!(b.recv(10, false, false, false, &never).unwrap().data, b"ef");
    }

    #[test]
    fn listening_and_connecting_by_name() {
        let names = Names::default();
        let server = UnixSocket::new(SOCK_STREAM);
        server.bind(&names, Name::Abstract(b"test".to_vec()), b"\x01\0\0test".to_vec()).unwrap();
        server.listen(4).unwrap();
        let client = UnixSocket::new(SOCK_STREAM);
        client.connect(&names, &Name::Abstract(b"test".to_vec()), 7, false, &never).unwrap();
        let conn = server.accept(true, &never).unwrap();
        client.send(&names, b"ping", Vec::new(), None, false, &never).unwrap();
        assert_eq!(conn.recv(10, false, false, false, &never).unwrap().data, b"ping");
        assert_eq!(conn.peer().unwrap().1, 7);
        assert_eq!(client.peer().unwrap().0, Some(b"\x01\0\0test".to_vec()));
        assert_eq!(server.accept(true, &never).err(), Some(EAGAIN));
        let other = UnixSocket::new(SOCK_STREAM);
        assert_eq!(other.connect(&names, &Name::Abstract(b"none".to_vec()), 1, false, &never), Err(ECONNREFUSED));
    }

    #[test]
    fn datagrams_keep_their_boundaries() {
        let names = Names::default();
        let (a, b) = UnixSocket::pair(SOCK_DGRAM, 1);
        a.send(&names, b"one", Vec::new(), None, false, &never).unwrap();
        a.send(&names, b"three", Vec::new(), None, false, &never).unwrap();
        let first = b.recv(2, false, false, false, &never).unwrap();
        assert_eq!((first.data.as_slice(), first.truncated), (&b"on"[..], true));
        assert_eq!(b.recv(10, false, false, false, &never).unwrap().data, b"three");
    }

    #[test]
    fn a_runtime_server_gets_its_connections() {
        let names = Names::default();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        names.serve(Name::Path("/run/x".into()), Arc::new(move |s: Arc<UnixSocket>| g.lock().unwrap().push(s)));
        let client = UnixSocket::new(SOCK_STREAM);
        client.connect(&names, &Name::Path("/run/x".into()), 3, false, &never).unwrap();
        client.send(&names, b"hi", Vec::new(), None, false, &never).unwrap();
        let server = got.lock().unwrap().pop().unwrap();
        assert_eq!(server.recv(10, false, false, false, &never).unwrap().data, b"hi");
        assert!(names.is_socket_path("/run/x"));
    }
}
