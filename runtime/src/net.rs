//! Sockets: the guest's TCP and UDP sockets are host sockets (IPv4 and
//! IPv6), so programs reach the network as the host does (docs/abi.md,
//! "Networking"). Also poll and select, over sockets, pipes and files.

use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::abi::errno::*;
use crate::abi::o;
use crate::fs::{Fd, Open};
use crate::memory::{self as mem, SysResult};
use crate::process::Process;

pub const AF_INET: i64 = 2;
pub const AF_INET6: i64 = 10;
const SOCK_STREAM: i64 = 1;
const SOCK_DGRAM: i64 = 2;
const SOCK_NONBLOCK: i64 = 0o4000;
const SOCK_CLOEXEC: i64 = 0o2000000;
const MSG_NOSIGNAL: i32 = 0x4000;

const EAFNOSUPPORT: i64 = 97;
const ENOTSOCK: i64 = 88;
const ENOPROTOOPT: i64 = 92;

/// A guest socket. Operations go straight to the host socket; blocking or
/// not follows the guest descriptor's O_NONBLOCK at each call.
pub struct GuestSocket {
    pub sock: Socket,
    pub kind: i64,
    nonblocking: Mutex<bool>,
}

impl GuestSocket {
    fn blocking(&self, nonblock: bool) -> SysResult<()> {
        let mut current = self.nonblocking.lock().unwrap();
        if *current != nonblock {
            self.sock.set_nonblocking(nonblock).map_err(|e| errno(&e))?;
            *current = nonblock;
        }
        Ok(())
    }
}

/// The guest errno for a host socket error. Linux hosts report the same
/// numbers the guest uses; elsewhere, map by kind.
pub fn errno(e: &std::io::Error) -> i64 {
    if cfg!(target_os = "linux")
        && let Some(n) = e.raw_os_error()
    {
        return n as i64;
    }
    use std::io::ErrorKind::*;
    match e.kind() {
        WouldBlock => EAGAIN,
        ConnectionRefused => 111,
        ConnectionReset => 104,
        ConnectionAborted => 103,
        NotConnected => 107,
        AddrInUse => 98,
        AddrNotAvailable => 99,
        BrokenPipe => EPIPE,
        TimedOut => ETIMEDOUT,
        Interrupted => EINTR,
        HostUnreachable => 113,
        NetworkUnreachable => 101,
        _ => crate::fs::errno(e),
    }
}

/// Decodes a guest `struct sockaddr_in` / `sockaddr_in6`.
fn read_sockaddr(data: &[u8], addr: u64, len: u64) -> SysResult<SockAddr> {
    let b = mem::slice(data, addr, len)?;
    let family = u16::from_le_bytes(b.get(..2).ok_or(EINVAL)?.try_into().unwrap()) as i64;
    let port = |b: &[u8]| u16::from_be_bytes([b[2], b[3]]);
    match family {
        AF_INET if b.len() >= 8 => {
            let ip = Ipv4Addr::new(b[4], b[5], b[6], b[7]);
            Ok(SocketAddr::V4(SocketAddrV4::new(ip, port(b))).into())
        }
        AF_INET6 if b.len() >= 24 => {
            let flow = u32::from_be_bytes(b[4..8].try_into().unwrap());
            let ip: [u8; 16] = b[8..24].try_into().unwrap();
            let scope = b.get(24..28).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()));
            Ok(SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(ip), port(b), flow, scope)).into())
        }
        AF_INET | AF_INET6 => Err(EINVAL),
        _ => Err(EAFNOSUPPORT),
    }
}

/// Encodes a socket address as the guest's `struct sockaddr_*`.
fn sockaddr_bytes(a: &SockAddr) -> Vec<u8> {
    match a.as_socket() {
        Some(SocketAddr::V4(v4)) => {
            let mut b = vec![0u8; 16];
            b[..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
            b[2..4].copy_from_slice(&v4.port().to_be_bytes());
            b[4..8].copy_from_slice(&v4.ip().octets());
            b
        }
        Some(SocketAddr::V6(v6)) => {
            let mut b = vec![0u8; 28];
            b[..2].copy_from_slice(&(AF_INET6 as u16).to_le_bytes());
            b[2..4].copy_from_slice(&v6.port().to_be_bytes());
            b[4..8].copy_from_slice(&v6.flowinfo().to_be_bytes());
            b[8..24].copy_from_slice(&v6.ip().octets());
            b[24..28].copy_from_slice(&v6.scope_id().to_le_bytes());
            b
        }
        None => vec![0u8; 2],
    }
}

/// Writes an address to a guest (addr, socklen_t *len) pair, truncated to
/// the room given, and the full length to *len. A null addr writes nothing.
fn write_sockaddr(data: &mut [u8], addr: u64, len_at: u64, a: &SockAddr) -> SysResult<()> {
    if addr == 0 || len_at == 0 {
        return Ok(());
    }
    let bytes = sockaddr_bytes(a);
    let room = u32::from_le_bytes(mem::slice(data, len_at, 4)?.try_into().unwrap()) as usize;
    mem::write(data, addr, &bytes[..bytes.len().min(room)])?;
    mem::write(data, len_at, &(bytes.len() as u32).to_le_bytes())
}

/// recv into guest memory (socket2 wants MaybeUninit).
fn recv_into(s: &Socket, buf: &mut [u8], flags: i32) -> std::io::Result<(usize, Option<SockAddr>)> {
    // SAFETY: MaybeUninit<u8> has u8's layout, and the buffer is initialized.
    let uninit = unsafe { &mut *(buf as *mut [u8] as *mut [MaybeUninit<u8>]) };
    match s.recv_from_with_flags(uninit, flags) {
        Ok((n, from)) => Ok((n, Some(from))),
        // Connected stream sockets have no source address to report.
        Err(e) if e.raw_os_error() == Some(22) => s.recv_with_flags(uninit, flags).map(|n| (n, None)),
        Err(e) => Err(e),
    }
}

impl Process {
    /// The socket behind `fd` and whether the descriptor is non-blocking.
    /// Socket I/O can block, so it happens without holding the descriptor.
    fn socket_of(&self, fd: i64) -> SysResult<(Arc<GuestSocket>, bool)> {
        let fd = self.fds.get(fd)?;
        let nonblock = fd.flags & o::NONBLOCK != 0;
        match &*fd.open.lock().unwrap() {
            Open::Socket(s) => Ok((s.clone(), nonblock)),
            _ => Err(ENOTSOCK),
        }
    }

    /// The socket behind `fd`, if it is one, set blocking as the guest
    /// asked: for read and write on any descriptor.
    pub fn maybe_socket(&self, fd: i64) -> SysResult<Option<Arc<GuestSocket>>> {
        match self.socket_of(fd) {
            Ok((s, nonblock)) => {
                s.blocking(nonblock)?;
                Ok(Some(s))
            }
            Err(ENOTSOCK) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn ready_socket(&self, fd: i64) -> SysResult<Arc<GuestSocket>> {
        let (s, nonblock) = self.socket_of(fd)?;
        s.blocking(nonblock)?;
        Ok(s)
    }

    pub fn socket(&mut self, domain: i64, ty: i64, protocol: i64) -> SysResult<i64> {
        let domain = match domain {
            AF_INET => Domain::IPV4,
            AF_INET6 => Domain::IPV6,
            // AF_UNIX, AF_NETLINK...: not yet. glibc copes (no nscd, and
            // getaddrinfo assumes both address families are configured).
            _ => return Err(EAFNOSUPPORT),
        };
        let kind = ty & 0xf;
        let t = match kind {
            SOCK_STREAM => Type::STREAM,
            SOCK_DGRAM => Type::DGRAM,
            _ => return Err(EINVAL),
        };
        let protocol = (protocol != 0).then(|| Protocol::from(protocol as i32));
        let sock = Socket::new(domain, t, protocol).map_err(|e| errno(&e))?;
        let s = GuestSocket { sock, kind, nonblocking: Mutex::new(false) };
        let flags = 2 /* O_RDWR */ | if ty & SOCK_NONBLOCK != 0 { o::NONBLOCK } else { 0 };
        let fd = Fd { open: Arc::new(Mutex::new(Open::Socket(Arc::new(s)))), cloexec: ty & SOCK_CLOEXEC != 0, flags };
        self.fds.insert(fd, 0)
    }

    /// socketpair(AF_UNIX, type, 0, sv): a connected pair of host Unix
    /// sockets (libuv connects a child's stdin, stdout and stderr with them).
    pub fn socketpair(&mut self, data: &mut [u8], domain: i64, ty: i64, sv: u64) -> SysResult<i64> {
        const AF_UNIX: i64 = 1;
        const SOCK_SEQPACKET: i64 = 5;
        if domain != AF_UNIX {
            return Err(EAFNOSUPPORT);
        }
        let kind = ty & 0xf;
        let host = match kind {
            SOCK_STREAM => libc::SOCK_STREAM,
            SOCK_DGRAM => libc::SOCK_DGRAM,
            SOCK_SEQPACKET => libc::SOCK_SEQPACKET,
            _ => return Err(EINVAL),
        };
        let mut raw = [0i32; 2];
        // SAFETY: `raw` has room for the two descriptors.
        if unsafe { libc::socketpair(libc::AF_UNIX, host | libc::SOCK_CLOEXEC, 0, raw.as_mut_ptr()) } < 0 {
            return Err(errno(&std::io::Error::last_os_error()));
        }
        let mut pair = [0u8; 8];
        for (k, raw) in raw.into_iter().enumerate() {
            // SAFETY: a descriptor socketpair just made, owned from here on.
            let sock = unsafe { <Socket as std::os::fd::FromRawFd>::from_raw_fd(raw) };
            let s = GuestSocket { sock, kind, nonblocking: Mutex::new(false) };
            let flags = 2 /* O_RDWR */ | if ty & SOCK_NONBLOCK != 0 { o::NONBLOCK } else { 0 };
            let fd = Fd { open: Arc::new(Mutex::new(Open::Socket(Arc::new(s)))), cloexec: ty & SOCK_CLOEXEC != 0, flags };
            let fd = self.fds.insert(fd, 0)?;
            pair[4 * k..4 * k + 4].copy_from_slice(&(fd as i32).to_le_bytes());
        }
        mem::write(data, sv, &pair).map(|_| 0)
    }

    pub fn connect(&mut self, data: &[u8], fd: i64, addr: u64, len: u64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        let a = read_sockaddr(data, addr, len)?;
        match s.sock.connect(&a) {
            Ok(()) => Ok(0),
            // A non-blocking connect in progress: EINPROGRESS, as on Linux.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(115),
            Err(e) => Err(errno(&e)),
        }
    }

    pub fn bind(&mut self, data: &[u8], fd: i64, addr: u64, len: u64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        let a = read_sockaddr(data, addr, len)?;
        s.sock.bind(&a).map(|_| 0).map_err(|e| errno(&e))
    }

    pub fn listen(&mut self, fd: i64, backlog: i64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        s.sock.listen(backlog as i32).map(|_| 0).map_err(|e| errno(&e))
    }

    pub fn accept(&mut self, data: &mut [u8], fd: i64, addr: u64, len_at: u64, flags: i64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        let (sock, from) = s.sock.accept().map_err(|e| errno(&e))?;
        write_sockaddr(data, addr, len_at, &from)?;
        let g = GuestSocket { sock, kind: s.kind, nonblocking: Mutex::new(false) };
        let fl = 2 | if flags & SOCK_NONBLOCK != 0 { o::NONBLOCK } else { 0 };
        let fd = Fd { open: Arc::new(Mutex::new(Open::Socket(Arc::new(g)))), cloexec: flags & SOCK_CLOEXEC != 0, flags: fl };
        self.fds.insert(fd, 0)
    }

    pub fn sock_name(&mut self, data: &mut [u8], fd: i64, addr: u64, len_at: u64, peer: bool) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        let a = if peer { s.sock.peer_addr() } else { s.sock.local_addr() }.map_err(|e| errno(&e))?;
        write_sockaddr(data, addr, len_at, &a).map(|_| 0)
    }

    pub fn sendto(&mut self, data: &[u8], fd: i64, buf: u64, len: u64, flags: i64, addr: u64, alen: u64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        let bytes = mem::slice(data, buf, len)?;
        // SIGPIPE isn't raised for sockets yet; a broken connection is EPIPE.
        let flags = flags as i32 & !MSG_NOSIGNAL;
        let r = if addr != 0 {
            let to = read_sockaddr(data, addr, alen)?;
            s.sock.send_to_with_flags(bytes, &to, flags)
        } else {
            s.sock.send_with_flags(bytes, flags)
        };
        r.map(|n| n as i64).map_err(|e| errno(&e))
    }

    pub fn recvfrom(&mut self, data: &mut [u8], fd: i64, buf: u64, len: u64, flags: i64, addr: u64, len_at: u64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        let (n, from) = recv_into(&s.sock, mem::slice_mut(data, buf, len)?, flags as i32).map_err(|e| errno(&e))?;
        if let Some(from) = from {
            write_sockaddr(data, addr, len_at, &from)?;
        }
        Ok(n as i64)
    }

    /// sendmsg: the message's iovecs gathered into one send.
    pub fn sendmsg(&mut self, data: &[u8], fd: i64, msg: u64, flags: i64) -> SysResult<i64> {
        let (name, namelen) = (mem::read_u64(data, msg)?, mem::read_u64(data, msg + 8)? as u32 as u64);
        let (iov, iovlen) = (mem::read_u64(data, msg + 16)?, mem::read_u64(data, msg + 24)?);
        let mut bytes = Vec::new();
        for (base, len) in mem::iovecs(data, iov, iovlen)? {
            bytes.extend_from_slice(mem::slice(data, base, len)?);
        }
        let s = self.ready_socket(fd)?;
        let flags = flags as i32 & !MSG_NOSIGNAL;
        let r = if name != 0 {
            let to = read_sockaddr(data, name, namelen)?;
            s.sock.send_to_with_flags(&bytes, &to, flags)
        } else {
            s.sock.send_with_flags(&bytes, flags)
        };
        r.map(|n| n as i64).map_err(|e| errno(&e))
    }

    /// recvmsg: one receive scattered over the message's iovecs. No
    /// control messages.
    pub fn recvmsg(&mut self, data: &mut [u8], fd: i64, msg: u64, flags: i64) -> SysResult<i64> {
        let (name, iov, iovlen) = (mem::read_u64(data, msg)?, mem::read_u64(data, msg + 16)?, mem::read_u64(data, msg + 24)?);
        let parts = mem::iovecs(data, iov, iovlen)?;
        let total: u64 = parts.iter().map(|p| p.1).sum();
        let mut buf = vec![0u8; total as usize];
        let s = self.ready_socket(fd)?;
        let (n, from) = recv_into(&s.sock, &mut buf, flags as i32).map_err(|e| errno(&e))?;
        let mut done = 0;
        for (base, len) in parts {
            let k = (len as usize).min(n - done);
            mem::write(data, base, &buf[done..done + k])?;
            done += k;
        }
        if let Some(from) = from {
            write_sockaddr(data, name, msg + 8, &from)?;
        } else {
            mem::write(data, msg + 8, &0u32.to_le_bytes())?;
        }
        mem::write(data, msg + 40, &0u64.to_le_bytes())?; // msg_controllen
        mem::write(data, msg + 48, &0i32.to_le_bytes())?; // msg_flags
        Ok(n as i64)
    }

    /// sendmmsg: sendmsg for each message (glibc's resolver sends its A and
    /// AAAA queries this way), recording each one's length.
    pub fn sendmmsg(&mut self, data: &mut [u8], fd: i64, vec: u64, count: u64, flags: i64) -> SysResult<i64> {
        let mut sent = 0;
        for k in 0..count.min(1024) {
            let msg = vec + k * 64; // struct mmsghdr: msghdr (56 bytes), msg_len, padding
            match self.sendmsg(data, fd, msg, flags) {
                Ok(n) => mem::write(data, msg + 56, &(n as u32).to_le_bytes())?,
                Err(e) if sent == 0 => return Err(e),
                Err(_) => break,
            }
            sent += 1;
        }
        Ok(sent)
    }

    pub fn shutdown(&mut self, fd: i64, how: i64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        let how = match how {
            0 => std::net::Shutdown::Read,
            1 => std::net::Shutdown::Write,
            2 => std::net::Shutdown::Both,
            _ => return Err(EINVAL),
        };
        s.sock.shutdown(how).map(|_| 0).map_err(|e| errno(&e))
    }

    pub fn setsockopt(&mut self, data: &[u8], fd: i64, level: i64, name: i64, val: u64, len: u64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        let int = mem::slice(data, val, len.min(4))?;
        let on = int.len() == 4 && u32::from_le_bytes(int.try_into().unwrap()) != 0;
        let v = if int.len() == 4 { u32::from_le_bytes(int.try_into().unwrap()) as usize } else { 0 };
        const SOL_SOCKET: i64 = 1;
        const IPPROTO_TCP: i64 = 6;
        const IPPROTO_IPV6: i64 = 41;
        let r = match (level, name) {
            (SOL_SOCKET, 2) => s.sock.set_reuse_address(on),
            (SOL_SOCKET, 9) => s.sock.set_keepalive(on),
            (SOL_SOCKET, 7) => s.sock.set_send_buffer_size(v),
            (SOL_SOCKET, 8) => s.sock.set_recv_buffer_size(v),
            (SOL_SOCKET, 6) => s.sock.set_broadcast(on),
            (IPPROTO_TCP, 1) => s.sock.set_tcp_nodelay(on),
            (IPPROTO_IPV6, 26) => s.sock.set_only_v6(on),
            // Others (timeouts are done with poll by most programs, TCP
            // keepalive tuning...) are accepted and ignored.
            _ => Ok(()),
        };
        r.map(|_| 0).map_err(|e| errno(&e))
    }

    pub fn getsockopt(&mut self, data: &mut [u8], fd: i64, level: i64, name: i64, val: u64, len_at: u64) -> SysResult<i64> {
        let s = self.ready_socket(fd)?;
        const SOL_SOCKET: i64 = 1;
        let v: i64 = match (level, name) {
            // SO_ERROR: a non-blocking connect's result.
            (SOL_SOCKET, 4) => match s.sock.take_error().map_err(|e| errno(&e))? {
                Some(e) => errno(&e),
                None => 0,
            },
            (SOL_SOCKET, 3) => s.kind, // SO_TYPE
            (SOL_SOCKET, 7) => s.sock.send_buffer_size().map_err(|e| errno(&e))? as i64,
            (SOL_SOCKET, 8) => s.sock.recv_buffer_size().map_err(|e| errno(&e))? as i64,
            (SOL_SOCKET, _) | (6, _) | (41, _) | (0, _) => 0,
            _ => return Err(ENOPROTOOPT),
        };
        if len_at != 0 {
            mem::write(data, val, &(v as i32).to_le_bytes())?;
            mem::write(data, len_at, &4u32.to_le_bytes())?;
        }
        Ok(0)
    }

    /// poll over `fds` (fd, events): the revents of each, waiting up to
    /// `timeout` (None: forever) for one to be ready. Sockets are polled on
    /// the host; files are always ready; pipes by their state.
    pub fn poll_fds(&self, fds: &[(i64, i16)], timeout: Option<Duration>) -> SysResult<Vec<i16>> {
        const POLLIN: i16 = 1;
        const POLLOUT: i16 = 4;
        const POLLERR: i16 = 8;
        const POLLHUP: i16 = 16;
        const POLLNVAL: i16 = 32;
        let deadline = timeout.map(|t| Instant::now() + t);
        let interrupted = self.interrupted();
        loop {
            let mut revents = vec![0i16; fds.len()];
            let mut host: Vec<(usize, Arc<GuestSocket>)> = Vec::new();
            for (k, &(fd, events)) in fds.iter().enumerate() {
                if fd < 0 {
                    continue;
                }
                let Ok(d) = self.fds.get(fd) else {
                    revents[k] = POLLNVAL;
                    continue;
                };
                match &*d.open.lock().unwrap() {
                    Open::Socket(s) => host.push((k, s.clone())),
                    Open::EventFd(e) => {
                        let (r, w) = e.readiness();
                        revents[k] = events & ((if r { POLLIN } else { 0 }) | (if w { POLLOUT } else { 0 }));
                    }
                    // (An epoll instance inside another isn't modelled: never ready.)
                    Open::Epoll(_) => {}
                    Open::Pipe(end) => {
                        let (r, w, no_writers, no_readers) = end.pipe.readiness();
                        let mut ev = 0;
                        if !end.write && r {
                            ev |= POLLIN;
                        }
                        if !end.write && no_writers {
                            ev |= POLLHUP;
                        }
                        if end.write && w {
                            ev |= POLLOUT;
                        }
                        if end.write && no_readers {
                            ev |= POLLERR;
                        }
                        revents[k] = ev & (events | POLLHUP | POLLERR);
                    }
                    _ => revents[k] = events & (POLLIN | POLLOUT),
                }
            }
            // Sockets: one host poll, waiting only if nothing else is ready.
            let others_ready = revents.iter().any(|&r| r != 0);
            let wait = if others_ready {
                Duration::ZERO
            } else {
                let left = deadline.map_or(Duration::from_millis(50), |d| d.saturating_duration_since(Instant::now()));
                // Short slices while pipes are involved or signals may come.
                left.min(Duration::from_millis(50))
            };
            if !host.is_empty() {
                let mut pfds: Vec<libc::pollfd> = host
                    .iter()
                    .map(|(k, s)| libc::pollfd {
                        fd: std::os::fd::AsRawFd::as_raw_fd(&s.sock),
                        events: fds[*k].1 & (POLLIN | POLLOUT),
                        revents: 0,
                    })
                    .collect();
                // SAFETY: pfds is a valid array of pollfd for its length.
                let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, wait.as_millis() as i32) };
                if n < 0 {
                    return Err(errno(&std::io::Error::last_os_error()));
                }
                for ((k, _), p) in host.iter().zip(&pfds) {
                    revents[*k] = p.revents;
                }
            } else if !others_ready && !wait.is_zero() {
                std::thread::sleep(wait.min(Duration::from_millis(10)));
            }
            if revents.iter().any(|&r| r != 0) || deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(revents);
            }
            if interrupted() {
                return Err(EINTR);
            }
        }
    }

    /// The epoll instance behind `fd` (EINVAL if it isn't one).
    pub fn epoll_of(&self, fd: i64) -> SysResult<Arc<crate::events::Epoll>> {
        match &*self.fds.get(fd)?.open.lock().unwrap() {
            Open::Epoll(e) => Ok(e.clone()),
            _ => Err(EINVAL),
        }
    }

    /// epoll_pwait: waits until descriptors of the interest list are ready
    /// (see events.rs), and writes up to `max` of them to `events_at`.
    pub fn epoll_wait(&mut self, data: &mut [u8], epfd: i64, events_at: u64, max: i64, timeout: Option<Duration>) -> SysResult<i64> {
        use crate::events::*;
        if max <= 0 {
            return Err(EINVAL);
        }
        let epoll = self.epoll_of(epfd)?;
        // Waited on: open descriptors with something to wait for (a
        // one-shot one reported already has nothing, until EPOLL_CTL_MOD).
        let list: Vec<(i64, u32, u64)> = {
            let mut interest = epoll.interest.lock().unwrap();
            // Closed since they were added: gone, as on Linux when the last
            // reference to the file goes.
            interest.retain(|&fd, _| self.fds.get(fd).is_ok());
            interest.iter().filter(|(_, (ev, _))| ev & (EPOLLIN | EPOLLOUT | EPOLLPRI | EPOLLRDHUP) != 0).map(|(&fd, &(ev, d))| (fd, ev, d)).collect()
        };
        let fds: Vec<(i64, i16)> = list.iter().map(|&(fd, ev, _)| (fd, (ev & (EPOLLIN | EPOLLOUT | EPOLLPRI)) as i16)).collect();
        let revents = self.poll_fds(&fds, timeout)?;
        let mut count = 0;
        let mut interest = epoll.interest.lock().unwrap();
        for (&(fd, ev, user), &r) in list.iter().zip(&revents) {
            if r == 0 || count >= max {
                continue;
            }
            // poll's POLLIN, POLLPRI, POLLOUT, POLLERR and POLLHUP are epoll's.
            let mut out = r as u16 as u32 & (EPOLLIN | EPOLLPRI | EPOLLOUT | EPOLLERR | EPOLLHUP);
            if out & EPOLLHUP != 0 && ev & EPOLLRDHUP != 0 {
                out |= EPOLLRDHUP;
            }
            let at = events_at + count as u64 * EVENT_SIZE;
            mem::write(data, at, &out.to_le_bytes())?;
            mem::write(data, at + 8, &user.to_le_bytes())?;
            count += 1;
            if ev & EPOLLONESHOT != 0
                && let Some(entry) = interest.get_mut(&fd)
            {
                entry.0 = EPOLLONESHOT;
            }
        }
        Ok(count)
    }

    /// ppoll(fds, nfds, timeout, sigmask, sigsetsize).
    pub fn ppoll(&mut self, data: &mut [u8], fds_at: u64, nfds: u64, timeout_at: u64) -> SysResult<i64> {
        let mut fds = Vec::new();
        for k in 0..nfds.min(4096) {
            let b = mem::slice(data, fds_at + k * 8, 8)?;
            fds.push((i32::from_le_bytes(b[..4].try_into().unwrap()) as i64, i16::from_le_bytes([b[4], b[5]])));
        }
        let timeout = read_timeout(data, timeout_at)?;
        let revents = self.poll_fds(&fds, timeout)?;
        for (k, r) in revents.iter().enumerate() {
            mem::write(data, fds_at + k as u64 * 8 + 6, &r.to_le_bytes())?;
        }
        Ok(revents.iter().filter(|&&r| r != 0).count() as i64)
    }

    /// pselect6(nfds, readfds, writefds, exceptfds, timeout, sigmask), by way
    /// of poll.
    pub fn pselect(&mut self, data: &mut [u8], nfds: i64, sets: [u64; 3], timeout_at: u64) -> SysResult<i64> {
        const POLLIN: i16 = 1;
        const POLLOUT: i16 = 4;
        let nfds = nfds.clamp(0, 1024) as u64;
        let bit = |data: &[u8], set: u64, fd: u64| -> SysResult<bool> {
            Ok(set != 0 && mem::slice(data, set + fd / 8, 1)?[0] & (1 << (fd % 8)) != 0)
        };
        let mut fds = Vec::new();
        for fd in 0..nfds {
            let mut ev = 0;
            if bit(data, sets[0], fd)? {
                ev |= POLLIN;
            }
            if bit(data, sets[1], fd)? {
                ev |= POLLOUT;
            }
            if ev != 0 {
                fds.push((fd as i64, ev));
            }
        }
        let timeout = read_timeout(data, timeout_at)?;
        let revents = self.poll_fds(&fds, timeout)?;
        for set in sets.into_iter().filter(|&s| s != 0) {
            mem::write(data, set, &vec![0u8; nfds.div_ceil(8) as usize])?;
        }
        let mut count = 0;
        for (&(fd, _), &r) in fds.iter().zip(&revents) {
            let fd = fd as u64;
            for (set, want) in [(sets[0], POLLIN | 8 | 16), (sets[1], POLLOUT | 8)] {
                if set != 0 && r & want != 0 {
                    let byte = mem::slice(data, set + fd / 8, 1)?[0] | (1 << (fd % 8));
                    mem::write(data, set + fd / 8, &[byte])?;
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

/// A `struct timespec *` timeout: None for a null pointer (wait forever).
pub(crate) fn read_timeout(data: &[u8], at: u64) -> SysResult<Option<Duration>> {
    if at == 0 {
        return Ok(None);
    }
    let secs = mem::read_u64(data, at)? as i64;
    let nanos = mem::read_u64(data, at + 8)? as i64;
    if secs < 0 || !(0..1_000_000_000).contains(&nanos) {
        return Err(EINVAL);
    }
    Ok(Some(Duration::new(secs as u64, nanos as u32)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockaddrs_round_trip() {
        for a in ["127.0.0.1:8080", "[::1]:443", "[fe80::1%3]:53"] {
            let sa: SockAddr = a.parse::<SocketAddr>().unwrap().into();
            let bytes = sockaddr_bytes(&sa);
            let back = read_sockaddr(&bytes, 0, bytes.len() as u64).unwrap();
            assert_eq!(back.as_socket(), sa.as_socket(), "{a}");
        }
        let v4 = sockaddr_bytes(&"10.1.2.3:80".parse::<SocketAddr>().unwrap().into());
        assert_eq!(&v4[..8], &[2, 0, 0, 80, 10, 1, 2, 3], "family (LE), port (BE), address");
    }
}

/// read() on a socket.
pub fn recv(s: &GuestSocket, buf: &mut [u8]) -> SysResult<i64> {
    recv_into(&s.sock, buf, 0).map(|(n, _)| n as i64).map_err(|e| errno(&e))
}

/// write() on a socket.
pub fn send(s: &GuestSocket, buf: &[u8]) -> SysResult<i64> {
    s.sock.send(buf).map(|n| n as i64).map_err(|e| errno(&e))
}

/// Bytes waiting to be read (FIONREAD).
pub fn pending(s: &GuestSocket) -> SysResult<usize> {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD writes one int.
    let r = unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&s.sock), libc::FIONREAD, &mut n) };
    if r < 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    Ok(n.max(0) as usize)
}
