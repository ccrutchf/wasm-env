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

pub const AF_UNIX: i64 = 1;
pub const AF_INET: i64 = 2;
pub const AF_INET6: i64 = 10;
const SOCK_STREAM: i64 = 1;
const SOCK_DGRAM: i64 = 2;
const SOCK_SEQPACKET: i64 = 5;
const SOCK_NONBLOCK: i64 = 0o4000;
const SOCK_CLOEXEC: i64 = 0o2000000;
const MSG_NOSIGNAL: i32 = 0x4000;

const EAFNOSUPPORT: i64 = 97;
const ENOTSOCK: i64 = 88;
const ENOPROTOOPT: i64 = 92;
const ENAMETOOLONG: i64 = 36;

/// A guest socket. Operations go straight to the host socket; blocking or
/// not follows the guest descriptor's O_NONBLOCK at each call.
pub struct GuestSocket {
    pub sock: Socket,
    pub kind: i64,
    nonblocking: Mutex<bool>,
    /// A Unix socket's address as the guest bound it, which getsockname
    /// reports (the host's is under the root).
    name: Mutex<Option<Vec<u8>>>,
}

impl GuestSocket {
    /// A guest socket for a host socket, blocking or not as the host
    /// socket is now (one passed in may be either).
    pub fn new(sock: Socket, kind: i64) -> Self {
        #[cfg(unix)]
        let nonblocking = host_nonblocking(std::os::fd::AsRawFd::as_raw_fd(&sock));
        #[cfg(not(unix))]
        let nonblocking = false; // (Windows can't ask; new sockets block)
        GuestSocket { sock, kind, nonblocking: Mutex::new(nonblocking), name: Mutex::new(None) }
    }

    fn blocking(&self, nonblock: bool) -> SysResult<()> {
        let mut current = self.nonblocking.lock().unwrap();
        if *current != nonblock {
            self.sock.set_nonblocking(nonblock).map_err(|e| errno(&e))?;
            *current = nonblock;
        }
        Ok(())
    }
}

/// Whether a host descriptor is non-blocking.
#[cfg(unix)]
pub fn host_nonblocking(fd: std::os::fd::RawFd) -> bool {
    // SAFETY: F_GETFL reads the descriptor's flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    flags >= 0 && flags & libc::O_NONBLOCK != 0
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

/// A Unix socket address whose sun_path is `path`: a pathname (with or
/// without its NUL), an abstract name (starting with NUL), or nothing
/// (unnamed).
fn unix_sockaddr(path: &[u8]) -> SysResult<SockAddr> {
    const SUN_PATH: usize = 108;
    if path.len() > SUN_PATH {
        return Err(ENAMETOOLONG);
    }
    // SAFETY: writes the family and `path` inside the (zeroed, larger)
    // storage, and their length.
    let ((), a) = unsafe {
        SockAddr::try_init(|storage, len| {
            let p = storage.cast::<u8>();
            std::ptr::copy_nonoverlapping((AF_UNIX as u16).to_ne_bytes().as_ptr(), p, 2);
            std::ptr::copy_nonoverlapping(path.as_ptr(), p.add(2), path.len());
            *len = (2 + path.len()) as _;
            Ok(())
        })
    }
    .map_err(|e| errno(&e))?;
    Ok(a)
}

/// A Unix socket address's sun_path bytes.
fn unix_path(a: &SockAddr) -> &[u8] {
    // SAFETY: the address's first `len` bytes are initialized.
    let all = unsafe { std::slice::from_raw_parts(a.as_ptr().cast::<u8>(), a.len() as usize) };
    &all[all.len().min(2)..]
}

/// Encodes a socket address as the guest's `struct sockaddr_*`. A Unix
/// socket's pathname under the root is the guest's path.
fn sockaddr_bytes(a: &SockAddr, fs: &crate::fs::Fs) -> Vec<u8> {
    if a.domain() == Domain::UNIX {
        let path = unix_path(a);
        let mut b = (AF_UNIX as u16).to_le_bytes().to_vec();
        if path.first().is_some_and(|&c| c != 0) {
            let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
            // SAFETY: bytes the host kernel reported as a path.
            let host = std::path::Path::new(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(&path[..end]) });
            match fs.guest(host) {
                Some(guest) => b.extend_from_slice(guest.as_bytes()),
                None => b.extend_from_slice(&path[..end]),
            }
            b.push(0);
        } else {
            b.extend_from_slice(path);
        }
        return b;
    }
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
fn write_sockaddr(data: &mut [u8], addr: u64, len_at: u64, bytes: &[u8]) -> SysResult<()> {
    if addr == 0 || len_at == 0 {
        return Ok(());
    }
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
    /// Decodes a guest socket address. A Unix socket's pathname is the
    /// guest's, under the root (`creating`, for bind, doesn't follow a
    /// symlink in its last component). A host path too long for sun_path is
    /// named through a descriptor of its directory, returned too: keep it
    /// until the address is used.
    fn sockaddr(&self, data: &[u8], addr: u64, len: u64, creating: bool) -> SysResult<(SockAddr, Option<std::fs::File>)> {
        let b = mem::slice(data, addr, len)?;
        let family = u16::from_le_bytes(b.get(..2).ok_or(EINVAL)?.try_into().unwrap()) as i64;
        if family != AF_UNIX {
            return read_sockaddr(data, addr, len).map(|a| (a, None));
        }
        let path = &b[2..];
        // Unnamed, or an abstract name: the host's as they are.
        if path.first().is_none_or(|&c| c == 0) {
            return Ok((unix_sockaddr(path)?, None));
        }
        let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
        let guest = std::str::from_utf8(&path[..end]).map_err(|_| EINVAL)?;
        let resolved = self.fs.resolve(&self.fs.cwd(), guest, !creating)?;
        // A path forwarded to a host socket (the compositor's) leads there.
        let host = match self.kernel.forwarded(&resolved) {
            Some(host) if !creating => host,
            _ => self.fs.host(&resolved),
        };
        let bytes = host.as_os_str().as_encoded_bytes();
        if bytes.len() < 108 {
            return Ok((unix_sockaddr(bytes)?, None));
        }
        #[cfg(not(target_os = "linux"))]
        return Err(ENAMETOOLONG);
        // (Linux: through /proc/self/fd.)
        #[cfg(target_os = "linux")]
        {
            let (Some(dir), Some(name)) = (host.parent(), host.file_name().and_then(|n| n.to_str())) else {
                return Err(ENAMETOOLONG);
            };
            let dir = std::fs::File::open(dir).map_err(|e| crate::fs::errno(&e))?;
            let short = format!("/proc/self/fd/{}/{name}", std::os::fd::AsRawFd::as_raw_fd(&dir));
            Ok((unix_sockaddr(short.as_bytes())?, Some(dir)))
        }
    }

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
        // Unix sockets are the runtime's own (unix.rs).
        if domain == AF_UNIX {
            let kind = ty & 0xf;
            if !matches!(kind, SOCK_STREAM | SOCK_DGRAM | SOCK_SEQPACKET) {
                return Err(EINVAL);
            }
            let open = Open::Unix(crate::unix::UnixSocket::new(kind));
            let flags = 2 /* O_RDWR */ | if ty & SOCK_NONBLOCK != 0 { o::NONBLOCK } else { 0 };
            return self.fds.insert(Fd { open: Arc::new(Mutex::new(open)), cloexec: ty & SOCK_CLOEXEC != 0, flags }, 0);
        }
        let domain = match domain {
            AF_INET => Domain::IPV4,
            AF_INET6 => Domain::IPV6,
            // AF_NETLINK...: not yet. glibc copes (no nscd, and getaddrinfo
            // assumes both address families are configured).
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
        let s = GuestSocket::new(sock, kind);
        let flags = 2 /* O_RDWR */ | if ty & SOCK_NONBLOCK != 0 { o::NONBLOCK } else { 0 };
        let fd = Fd { open: Arc::new(Mutex::new(Open::Socket(Arc::new(s)))), cloexec: ty & SOCK_CLOEXEC != 0, flags };
        self.fds.insert(fd, 0)
    }

    /// socketpair(AF_UNIX, type, 0, sv): a connected pair of host Unix
    /// sockets (libuv connects a child's stdin, stdout and stderr with them).
    pub fn socketpair(&mut self, data: &mut [u8], domain: i64, ty: i64, sv: u64) -> SysResult<i64> {
        if domain != AF_UNIX {
            return Err(EAFNOSUPPORT);
        }
        let kind = ty & 0xf;
        if !matches!(kind, SOCK_STREAM | SOCK_DGRAM | SOCK_SEQPACKET) {
            return Err(EINVAL);
        }
        // A pair of the runtime's own Unix sockets (unix.rs).
        let (a, b) = crate::unix::UnixSocket::pair(kind, self.pid);
        let mut pair = [0u8; 8];
        for (k, s) in [a, b].into_iter().enumerate() {
            let flags = 2 /* O_RDWR */ | if ty & SOCK_NONBLOCK != 0 { o::NONBLOCK } else { 0 };
            let fd = Fd { open: Arc::new(Mutex::new(Open::Unix(s))), cloexec: ty & SOCK_CLOEXEC != 0, flags };
            let fd = self.fds.insert(fd, 0)?;
            pair[4 * k..4 * k + 4].copy_from_slice(&(fd as i32).to_le_bytes());
        }
        mem::write(data, sv, &pair).map(|_| 0)
    }

    pub fn connect(&mut self, data: &[u8], fd: i64, addr: u64, len: u64) -> SysResult<i64> {
        if let Some((u, nonblock)) = self.unix_of(fd)? {
            return self.unix_connect(data, fd, &u, nonblock, addr, len);
        }
        let s = self.ready_socket(fd)?;
        let (a, _dir) = self.sockaddr(data, addr, len, false)?;
        match s.sock.connect(&a) {
            Ok(()) => Ok(0),
            // A non-blocking connect in progress: EINPROGRESS, as on Linux.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(115),
            Err(e) => Err(errno(&e)),
        }
    }

    pub fn bind(&mut self, data: &[u8], fd: i64, addr: u64, len: u64) -> SysResult<i64> {
        if let Some((u, _)) = self.unix_of(fd)? {
            return self.unix_bind(data, &u, addr, len);
        }
        let s = self.ready_socket(fd)?;
        let (a, _dir) = self.sockaddr(data, addr, len, true)?;
        s.sock.bind(&a).map_err(|e| errno(&e))?;
        // A Unix socket reports the name the guest gave it (up to its NUL).
        if a.domain() == Domain::UNIX {
            let b = mem::slice(data, addr, len)?;
            let path = &b[2..];
            let named = match path.first() {
                Some(0) | None => b.to_vec(),
                Some(_) => {
                    let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
                    [&b[..2 + end], &[0]].concat()
                }
            };
            *s.name.lock().unwrap() = Some(named);
        }
        Ok(0)
    }

    pub fn listen(&mut self, fd: i64, backlog: i64) -> SysResult<i64> {
        if let Some((u, _)) = self.unix_of(fd)? {
            return u.listen(backlog).map(|_| 0);
        }
        let s = self.ready_socket(fd)?;
        s.sock.listen(backlog as i32).map(|_| 0).map_err(|e| errno(&e))
    }

    pub fn accept(&mut self, data: &mut [u8], fd: i64, addr: u64, len_at: u64, flags: i64) -> SysResult<i64> {
        if let Some((u, nonblock)) = self.unix_of(fd)? {
            return self.unix_accept(data, &u, nonblock, addr, len_at, flags);
        }
        let s = self.ready_socket(fd)?;
        let (sock, from) = s.sock.accept().map_err(|e| errno(&e))?;
        write_sockaddr(data, addr, len_at, &sockaddr_bytes(&from, &self.fs))?;
        let g = GuestSocket::new(sock, s.kind);
        let fl = 2 | if flags & SOCK_NONBLOCK != 0 { o::NONBLOCK } else { 0 };
        let fd = Fd { open: Arc::new(Mutex::new(Open::Socket(Arc::new(g)))), cloexec: flags & SOCK_CLOEXEC != 0, flags: fl };
        self.fds.insert(fd, 0)
    }

    pub fn sock_name(&mut self, data: &mut [u8], fd: i64, addr: u64, len_at: u64, peer: bool) -> SysResult<i64> {
        if let Some((u, _)) = self.unix_of(fd)? {
            let a = if peer { u.peer()?.0 } else { u.address() };
            return write_sockaddr(data, addr, len_at, &a.unwrap_or(UNNAMED.to_vec())).map(|_| 0);
        }
        let s = self.ready_socket(fd)?;
        if !peer && let Some(name) = &*s.name.lock().unwrap() {
            return write_sockaddr(data, addr, len_at, name).map(|_| 0);
        }
        let a = if peer { s.sock.peer_addr() } else { s.sock.local_addr() }.map_err(|e| errno(&e))?;
        write_sockaddr(data, addr, len_at, &sockaddr_bytes(&a, &self.fs)).map(|_| 0)
    }

    pub fn sendto(&mut self, data: &[u8], fd: i64, buf: u64, len: u64, flags: i64, addr: u64, alen: u64) -> SysResult<i64> {
        if let Some((u, nonblock)) = self.unix_of(fd)? {
            let to = if addr != 0 { self.unix_name(data, addr, alen, false)?.map(|(n, _)| n) } else { None };
            let bytes = mem::slice(data, buf, len)?;
            return self.unix_send(&u, nonblock, bytes, Vec::new(), to, flags as i32);
        }
        let s = self.ready_socket(fd)?;
        let bytes = mem::slice(data, buf, len)?;
        // SIGPIPE isn't raised for sockets yet; a broken connection is EPIPE.
        let flags = flags as i32 & !MSG_NOSIGNAL;
        let r = if addr != 0 {
            let (to, _dir) = self.sockaddr(data, addr, alen, false)?;
            s.sock.send_to_with_flags(bytes, &to, flags)
        } else {
            s.sock.send_with_flags(bytes, flags)
        };
        r.map(|n| n as i64).map_err(|e| errno(&e))
    }

    pub fn recvfrom(&mut self, data: &mut [u8], fd: i64, buf: u64, len: u64, flags: i64, addr: u64, len_at: u64) -> SysResult<i64> {
        if let Some((u, nonblock)) = self.unix_of(fd)? {
            let got = self.unix_recv(&u, nonblock, len as usize, flags as i32)?;
            mem::write(data, buf, &got.data)?;
            if let Some(from) = &got.from {
                write_sockaddr(data, addr, len_at, from)?;
            }
            // (MSG_TRUNC: a datagram's whole length.)
            return Ok(if got.truncated && flags as i32 & MSG_TRUNC != 0 { got.full as i64 } else { got.data.len() as i64 });
        }
        let s = self.ready_socket(fd)?;
        let (n, from) = recv_into(&s.sock, mem::slice_mut(data, buf, len)?, flags as i32).map_err(|e| errno(&e))?;
        if let Some(from) = from {
            write_sockaddr(data, addr, len_at, &sockaddr_bytes(&from, &self.fs))?;
        }
        Ok(n as i64)
    }

    /// sendmsg: the message's iovecs gathered into one send, with the
    /// descriptors of its SCM_RIGHTS control messages passed as host
    /// descriptors (see `passable`). Other control messages are dropped.
    pub fn sendmsg(&mut self, data: &[u8], fd: i64, msg: u64, flags: i64) -> SysResult<i64> {
        let (name, namelen) = (mem::read_u64(data, msg)?, mem::read_u64(data, msg + 8)? as u32 as u64);
        let (iov, iovlen) = (mem::read_u64(data, msg + 16)?, mem::read_u64(data, msg + 24)?);
        let (control, controllen) = (mem::read_u64(data, msg + 32)?, mem::read_u64(data, msg + 40)?);
        let mut bytes = Vec::new();
        for (base, len) in mem::iovecs(data, iov, iovlen)? {
            bytes.extend_from_slice(mem::slice(data, base, len)?);
        }
        if let Some((u, nonblock)) = self.unix_of(fd)? {
            let to = if name != 0 { self.unix_name(data, name, namelen, false)?.map(|(n, _)| n) } else { None };
            let fds = if control != 0 && controllen != 0 { self.unix_passed(data, control, controllen)? } else { Vec::new() };
            return self.unix_send(&u, nonblock, &bytes, fds, to, flags as i32);
        }
        let s = self.ready_socket(fd)?;
        let flags = flags as i32 & !MSG_NOSIGNAL;
        let (to, _dir) = if name != 0 { self.sockaddr(data, name, namelen, false).map(|(a, d)| (Some(a), d))? } else { (None, None) };
        // (Descriptors pass over host sockets only between Unix processes.)
        #[cfg(unix)]
        if control != 0 && controllen != 0 {
            let fds = self.passed_fds(data, control, controllen)?;
            if !fds.is_empty() {
                return send_with_fds(&s.sock, &bytes, to.as_ref(), &fds, flags);
            }
        }
        let r = match &to {
            Some(to) => s.sock.send_to_with_flags(&bytes, to, flags),
            None => s.sock.send_with_flags(&bytes, flags),
        };
        r.map(|n| n as i64).map_err(|e| errno(&e))
    }

    /// recvmsg: one receive scattered over the message's iovecs. Descriptors
    /// passed in SCM_RIGHTS control messages become the guest's (see
    /// `adopt`); other control messages are copied as they are.
    pub fn recvmsg(&mut self, data: &mut [u8], fd: i64, msg: u64, flags: i64) -> SysResult<i64> {
        let (name, iov, iovlen) = (mem::read_u64(data, msg)?, mem::read_u64(data, msg + 16)?, mem::read_u64(data, msg + 24)?);
        let (control, controllen) = (mem::read_u64(data, msg + 32)?, mem::read_u64(data, msg + 40)?);
        let parts = mem::iovecs(data, iov, iovlen)?;
        let total: u64 = parts.iter().map(|p| p.1).sum();
        if let Some((u, nonblock)) = self.unix_of(fd)? {
            let got = self.unix_recv(&u, nonblock, total as usize, flags as i32)?;
            let mut done = 0;
            for &(base, len) in &parts {
                let k = (len as usize).min(got.data.len() - done);
                mem::write(data, base, &got.data[done..done + k])?;
                done += k;
            }
            match &got.from {
                Some(from) => write_sockaddr(data, name, msg + 8, from)?,
                None => mem::write(data, msg + 8, &0u32.to_le_bytes())?,
            }
            // Descriptors passed: installed as the guest's, as many as the
            // control buffer holds (the rest are closed: MSG_CTRUNC).
            let mut msg_flags = if got.truncated { MSG_TRUNC } else { 0 };
            let room = if control != 0 { (controllen as usize).saturating_sub(16) / 4 } else { 0 };
            let mut written = 0;
            if !got.fds.is_empty() {
                let n = got.fds.len().min(room);
                if n < got.fds.len() {
                    msg_flags |= MSG_CTRUNC;
                }
                if n > 0 {
                    let cloexec = flags as i32 & MSG_CMSG_CLOEXEC != 0;
                    let mut out = Vec::new();
                    out.extend_from_slice(&(16 + 4 * n as u64).to_le_bytes());
                    out.extend_from_slice(&SOL_SOCKET.to_le_bytes());
                    out.extend_from_slice(&SCM_RIGHTS.to_le_bytes());
                    for p in got.fds.into_iter().take(n) {
                        let guest = self.fds.insert(Fd { open: p.open, cloexec, flags: p.flags }, 0)?;
                        out.extend_from_slice(&(guest as i32).to_le_bytes());
                    }
                    out.resize(out.len().next_multiple_of(8).min(controllen as usize), 0);
                    mem::write(data, control, &out)?;
                    written = out.len() as u64;
                }
            }
            mem::write(data, msg + 40, &written.to_le_bytes())?; // msg_controllen
            mem::write(data, msg + 48, &msg_flags.to_le_bytes())?; // msg_flags
            return Ok(if got.truncated && flags as i32 & MSG_TRUNC != 0 { got.full as i64 } else { got.data.len() as i64 });
        }
        let mut buf = vec![0u8; total as usize];
        let s = self.ready_socket(fd)?;
        #[cfg(unix)]
        let (n, from, cmsgs, msg_flags) = if control != 0 && controllen != 0 {
            recv_with_control(&s.sock, &mut buf, controllen.min(1 << 16) as usize, flags as i32).map_err(|e| errno(&e))?
        } else {
            let (n, from) = recv_into(&s.sock, &mut buf, flags as i32).map_err(|e| errno(&e))?;
            (n, from, Vec::new(), 0)
        };
        #[cfg(not(unix))]
        let (n, from, msg_flags) = {
            let (n, from) = recv_into(&s.sock, &mut buf, flags as i32).map_err(|e| errno(&e))?;
            (n, from, 0i32)
        };
        let mut done = 0;
        for (base, len) in parts {
            let k = (len as usize).min(n - done);
            mem::write(data, base, &buf[done..done + k])?;
            done += k;
        }
        match from {
            Some(from) => write_sockaddr(data, name, msg + 8, &sockaddr_bytes(&from, &self.fs))?,
            None => mem::write(data, msg + 8, &0u32.to_le_bytes())?,
        }
        #[cfg(unix)]
        let out = self.guest_control(cmsgs, flags as i32 & MSG_CMSG_CLOEXEC != 0)?;
        #[cfg(not(unix))]
        let out: Vec<u8> = Vec::new();
        let written = out.len().min(controllen as usize);
        if written > 0 {
            mem::write(data, control, &out[..written])?;
        }
        mem::write(data, msg + 40, &(written as u64).to_le_bytes())?; // msg_controllen
        mem::write(data, msg + 48, &msg_flags.to_le_bytes())?; // msg_flags
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
        if let Some((u, _)) = self.unix_of(fd)? {
            return u.shutdown(how).map(|_| 0);
        }
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
        if self.unix_of(fd)?.is_some() {
            return Ok(0); // (SO_PASSCRED and the like: accepted)
        }
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
        if let Some((u, _)) = self.unix_of(fd)? {
            return self.unix_sockopt(data, &u, level, name, val, len_at);
        }
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
            // Host descriptors (sockets, others passed in), polled on the host:
            // (index, descriptor), and what keeps each open meanwhile.
            let mut host: Vec<(usize, Pollable)> = Vec::new();
            let mut keep: Vec<Arc<dyn std::any::Any + Send + Sync>> = Vec::new();
            for (k, &(fd, events)) in fds.iter().enumerate() {
                if fd < 0 {
                    continue;
                }
                let Ok(d) = self.fds.get(fd) else {
                    revents[k] = POLLNVAL;
                    continue;
                };
                match &*d.open.lock().unwrap() {
                    Open::Socket(s) => {
                        host.push((k, pollable(&s.sock)));
                        keep.push(s.clone());
                    }
                    #[cfg(unix)]
                    Open::Host(h) => {
                        host.push((k, std::os::fd::AsRawFd::as_raw_fd(&h.fd)));
                        keep.push(h.clone());
                    }
                    // (Other host handles on Windows: always ready.)
                    Open::Unix(u) => {
                        let (r, w, hup) = u.readiness();
                        let mut ev = 0;
                        if r {
                            ev |= POLLIN;
                        }
                        if w {
                            ev |= POLLOUT;
                        }
                        revents[k] = (ev & events) | if hup { POLLHUP } else { 0 };
                    }
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
                let wanted: Vec<(Pollable, i16)> = host.iter().map(|&(k, fd)| (fd, fds[k].1 & (POLLIN | POLLOUT))).collect();
                for ((k, _), r) in host.iter().zip(host_poll(&wanted, wait)?) {
                    revents[*k] = r;
                }
                drop(keep);
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
            // Edge-triggered on something whose readiness never changes (a
            // file, /proc/self/mountinfo, a device): one report, as on Linux,
            // until EPOLL_CTL_MOD. (Pipes, sockets and eventfds stay
            // level-triggered, which suits edge-triggered readers too: they
            // read until EAGAIN.) GLib's mount monitor spun on mountinfo.
            if ev & EPOLLET != 0
                && self.fds.get(fd).is_ok_and(|d| matches!(&*d.open.lock().unwrap(), Open::File { .. } | Open::Dir { .. } | Open::Device(_)))
                && let Some(entry) = interest.get_mut(&fd)
            {
                entry.0 &= !(EPOLLIN | EPOLLOUT | EPOLLPRI | EPOLLRDHUP);
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



// --- Unix sockets: the runtime's own (unix.rs) ------------------------------

const MSG_PEEK: i32 = 2;
const MSG_CTRUNC: i32 = 8;
const MSG_TRUNC: i32 = 0x20;
const MSG_DONTWAIT: i32 = 0x40;
const MSG_WAITALL: i32 = 0x100;

/// An unnamed Unix socket's address: the family alone.
const UNNAMED: [u8; 2] = [AF_UNIX as u8, 0];

impl Process {
    /// The runtime Unix socket behind `fd`, if it is one, and whether the
    /// descriptor is non-blocking.
    pub fn unix_of(&self, fd: i64) -> SysResult<Option<(Arc<crate::unix::UnixSocket>, bool)>> {
        let fd = self.fds.get(fd)?;
        let nonblock = fd.flags & o::NONBLOCK != 0;
        match &*fd.open.lock().unwrap() {
            Open::Unix(u) => Ok(Some((u.clone(), nonblock))),
            _ => Ok(None),
        }
    }

    /// A guest `struct sockaddr_un`: its name (a path resolved under the
    /// root, or an abstract name) and its bytes as given (up to a path's
    /// NUL); None for an unnamed address. `creating` (bind) doesn't follow
    /// a symlink in the last component.
    fn unix_name(&self, data: &[u8], addr: u64, len: u64, creating: bool) -> SysResult<Option<(crate::unix::Name, Vec<u8>)>> {
        use crate::unix::Name;
        let b = mem::slice(data, addr, len)?;
        if b.len() < 2 || u16::from_le_bytes([b[0], b[1]]) as i64 != AF_UNIX {
            return Err(EINVAL);
        }
        let path = &b[2..];
        match path.first() {
            None => Ok(None),
            Some(0) => Ok(Some((Name::Abstract(path[1..].to_vec()), b.to_vec()))),
            Some(_) => {
                let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
                let guest = std::str::from_utf8(&path[..end]).map_err(|_| EINVAL)?;
                let resolved = self.fs.resolve(&self.fs.cwd(), guest, !creating)?;
                Ok(Some((Name::Path(resolved), [&b[..2 + end], &[0]].concat())))
            }
        }
    }

    fn unix_bind(&mut self, data: &[u8], u: &Arc<crate::unix::UnixSocket>, addr: u64, len: u64) -> SysResult<i64> {
        use crate::unix::Name;
        // (An unnamed address: no name. Linux would pick an abstract one.)
        let Some((name, shown)) = self.unix_name(data, addr, len, true)? else { return Ok(0) };
        if let Name::Path(p) = &name {
            // The socket's file, as Linux makes one: taken if it exists.
            match std::fs::OpenOptions::new().write(true).create_new(true).open(self.fs.host(p)) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(98), // EADDRINUSE
                Err(e) => return Err(crate::fs::errno(&e)),
            }
        }
        u.bind(&self.kernel.unix, name, shown).map(|_| 0)
    }

    fn unix_connect(&mut self, data: &[u8], fd: i64, u: &Arc<crate::unix::UnixSocket>, nonblock: bool, addr: u64, len: u64) -> SysResult<i64> {
        use crate::unix::Name;
        let Some((name, _)) = self.unix_name(data, addr, len, false)? else { return Err(EINVAL) };
        if let Name::Path(p) = &name {
            // A path leading to a host socket (the host's compositor, with
            // --wayland=host): the description becomes a host socket.
            if let Some(host) = self.kernel.forwarded(p) {
                let t = match u.kind {
                    SOCK_DGRAM => Type::DGRAM,
                    SOCK_SEQPACKET => Type::SEQPACKET,
                    _ => Type::STREAM,
                };
                let sock = Socket::new(Domain::UNIX, t, None).map_err(|e| errno(&e))?;
                sock.connect(&SockAddr::unix(&host).map_err(|e| errno(&e))?).map_err(|e| errno(&e))?;
                *self.fds.get(fd)?.open.lock().unwrap() = Open::Socket(Arc::new(GuestSocket::new(sock, u.kind)));
                return Ok(0);
            }
            // Its file was removed: nothing listens there any more.
            if !self.fs.host(p).exists() {
                return Err(ENOENT);
            }
        }
        u.connect(&self.kernel.unix, &name, self.pid, nonblock, &self.interrupted()).map(|_| 0)
    }

    fn unix_accept(&mut self, data: &mut [u8], u: &crate::unix::UnixSocket, nonblock: bool, addr: u64, len_at: u64, flags: i64) -> SysResult<i64> {
        let s = u.accept(nonblock, &self.interrupted())?;
        let peer = s.peer().ok().and_then(|(a, _)| a).unwrap_or(UNNAMED.to_vec());
        write_sockaddr(data, addr, len_at, &peer)?;
        let fl = 2 | if flags & SOCK_NONBLOCK != 0 { o::NONBLOCK } else { 0 };
        let fd = Fd { open: Arc::new(Mutex::new(Open::Unix(s))), cloexec: flags & SOCK_CLOEXEC != 0, flags: fl };
        self.fds.insert(fd, 0)
    }

    /// The descriptors of the SCM_RIGHTS messages in a guest control
    /// buffer, as open file descriptions to pass.
    fn unix_passed(&self, data: &[u8], control: u64, len: u64) -> SysResult<Vec<crate::unix::Passed>> {
        let (mut at, end) = (control, control + len);
        let mut fds = Vec::new();
        while at + 16 <= end {
            let len = mem::read_u64(data, at)?;
            let head = mem::slice(data, at + 8, 8)?;
            let (level, ty) = (i32::from_le_bytes(head[..4].try_into().unwrap()), i32::from_le_bytes(head[4..].try_into().unwrap()));
            if len < 16 || at + len > end {
                return Err(EINVAL);
            }
            if level == SOL_SOCKET && ty == SCM_RIGHTS {
                for k in 0..(len - 16) / 4 {
                    let n = i32::from_le_bytes(mem::slice(data, at + 16 + 4 * k, 4)?.try_into().unwrap());
                    let fd = self.fds.get(n as i64)?;
                    fds.push(crate::unix::Passed { open: fd.open.clone(), flags: fd.flags });
                }
            }
            at += len.next_multiple_of(8);
        }
        Ok(fds)
    }

    /// A send on a runtime Unix socket: EPIPE raises SIGPIPE unless
    /// MSG_NOSIGNAL.
    fn unix_send(&mut self, u: &crate::unix::UnixSocket, nonblock: bool, bytes: &[u8], fds: Vec<crate::unix::Passed>, to: Option<crate::unix::Name>, flags: i32) -> SysResult<i64> {
        let nonblock = nonblock || flags & MSG_DONTWAIT != 0;
        let r = u.send(&self.kernel.unix, bytes, fds, to.as_ref(), nonblock, &self.interrupted());
        if r == Err(EPIPE) && flags & MSG_NOSIGNAL == 0 {
            self.raise_sigpipe();
        }
        r.map(|n| n as i64)
    }

    fn unix_recv(&self, u: &crate::unix::UnixSocket, nonblock: bool, len: usize, flags: i32) -> SysResult<crate::unix::Received> {
        let nonblock = nonblock || flags & MSG_DONTWAIT != 0;
        u.recv(len, flags & MSG_PEEK != 0, flags & MSG_WAITALL != 0, nonblock, &self.interrupted())
    }

    /// read() on a runtime Unix socket (descriptors sent with the bytes are
    /// closed, as Linux does).
    pub fn unix_read(&self, u: &crate::unix::UnixSocket, nonblock: bool, buf: &mut [u8]) -> SysResult<i64> {
        let got = self.unix_recv(u, nonblock, buf.len(), 0)?;
        buf[..got.data.len()].copy_from_slice(&got.data);
        Ok(got.data.len() as i64)
    }

    /// write() on a runtime Unix socket.
    pub fn unix_write(&mut self, u: &crate::unix::UnixSocket, nonblock: bool, buf: &[u8]) -> SysResult<i64> {
        self.unix_send(u, nonblock, buf, Vec::new(), None, 0)
    }

    fn unix_sockopt(&self, data: &mut [u8], u: &crate::unix::UnixSocket, level: i64, name: i64, val: u64, len_at: u64) -> SysResult<i64> {
        const SO_PEERCRED: i64 = 17;
        let v: i64 = match (level, name) {
            // struct ucred: the connecting process, and root (as the
            // environment's programs run).
            (1, SO_PEERCRED) => {
                let (_, pid) = u.peer()?;
                let mut cred = [0u8; 12];
                cred[..4].copy_from_slice(&pid.to_le_bytes());
                if len_at != 0 {
                    mem::write(data, val, &cred)?;
                    mem::write(data, len_at, &12u32.to_le_bytes())?;
                }
                return Ok(0);
            }
            (1, 3) => u.kind,                // SO_TYPE
            (1, 7) | (1, 8) => 212_992,      // SO_SNDBUF, SO_RCVBUF
            (1, 39) => AF_UNIX,              // SO_DOMAIN
            (1, _) => 0,                     // SO_ERROR, SO_PASSCRED...
            _ => return Err(ENOPROTOOPT),
        };
        if len_at != 0 {
            mem::write(data, val, &(v as i32).to_le_bytes())?;
            mem::write(data, len_at, &4u32.to_le_bytes())?;
        }
        Ok(0)
    }
}

// --- Passing descriptors (SCM_RIGHTS) ---------------------------------------

const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;
const MSG_CMSG_CLOEXEC: i32 = 0x4000_0000;

/// A control message received: descriptors passed (SCM_RIGHTS), or another
/// kind's bytes.
#[cfg(unix)]
enum Cmsg {
    Fds(Vec<std::os::fd::OwnedFd>),
    Raw(Vec<u8>),
}

/// Received: bytes, the sender's address, control messages (level, type,
/// body) and the message flags.
#[cfg(unix)]
type Received = (usize, Option<SockAddr>, Vec<(i32, i32, Cmsg)>, i32);

/// sendmsg on the host with `fds` passed in one SCM_RIGHTS message.
#[cfg(unix)]
fn send_with_fds(s: &Socket, bytes: &[u8], to: Option<&SockAddr>, fds: &[std::os::fd::OwnedFd], flags: i32) -> SysResult<i64> {
    use std::os::fd::AsRawFd;
    let data_len = (fds.len() * 4) as u32;
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE(data_len) } as usize;
    let mut control = vec![0u64; space.div_ceil(8)];
    let mut iov = libc::iovec { iov_base: bytes.as_ptr() as *mut _, iov_len: bytes.len() };
    // SAFETY: a zeroed msghdr is a valid empty one; the fields set below
    // point at buffers that outlive the call.
    let mut m: libc::msghdr = unsafe { std::mem::zeroed() };
    if let Some(to) = to {
        m.msg_name = to.as_ptr() as *mut _;
        m.msg_namelen = to.len();
    }
    m.msg_iov = &mut iov;
    m.msg_iovlen = 1;
    m.msg_control = control.as_mut_ptr().cast();
    m.msg_controllen = space as _;
    // SAFETY: the control buffer has room for one header and the
    // descriptors (CMSG_SPACE), and CMSG_FIRSTHDR points at its start.
    let n = unsafe {
        let c = libc::CMSG_FIRSTHDR(&m);
        (*c).cmsg_level = SOL_SOCKET;
        (*c).cmsg_type = SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(data_len) as _;
        let at = libc::CMSG_DATA(c).cast::<i32>();
        for (k, fd) in fds.iter().enumerate() {
            at.add(k).write_unaligned(fd.as_raw_fd());
        }
        libc::sendmsg(s.as_raw_fd(), &m, flags)
    };
    if n < 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    Ok(n as i64)
}

/// recvmsg on the host with room for `control_len` bytes of control
/// messages. Descriptors received are close-on-exec on the host (the
/// guest's own flag is separate).
#[cfg(unix)]
fn recv_with_control(s: &Socket, buf: &mut [u8], control_len: usize, flags: i32) -> std::io::Result<Received> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut control = vec![0u64; control_len.div_ceil(8)];
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    // SAFETY: as in send_with_fds; the address goes to the storage
    // try_init provides, whose length recvmsg sets.
    let ((n, m), from) = unsafe {
        SockAddr::try_init(|storage, len| {
            let mut m: libc::msghdr = std::mem::zeroed();
            m.msg_name = storage.cast();
            m.msg_namelen = *len;
            m.msg_iov = &mut iov;
            m.msg_iovlen = 1;
            m.msg_control = control.as_mut_ptr().cast();
            m.msg_controllen = control_len as _;
            #[cfg(target_os = "linux")]
            let flags = flags | libc::MSG_CMSG_CLOEXEC;
            let n = libc::recvmsg(s.as_raw_fd(), &mut m, flags);
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            *len = m.msg_namelen;
            Ok((n as usize, m))
        })
    }?;
    let mut cmsgs = Vec::new();
    // SAFETY: walks the control messages recvmsg wrote, within the buffer
    // (CMSG_NXTHDR checks the bounds); each SCM_RIGHTS descriptor is new
    // and owned from here on.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&m);
        while !c.is_null() {
            let (level, ty) = ((*c).cmsg_level, (*c).cmsg_type);
            let body = libc::CMSG_DATA(c);
            let len = (*c).cmsg_len as usize - (body as usize - c as usize);
            let msg = if level == SOL_SOCKET && ty == SCM_RIGHTS {
                let fds: Vec<OwnedFd> = (0..len / 4).map(|k| OwnedFd::from_raw_fd(body.cast::<i32>().add(k).read_unaligned())).collect();
                // Where recvmsg can't set it (MSG_CMSG_CLOEXEC is Linux's).
                #[cfg(not(target_os = "linux"))]
                for fd in &fds {
                    libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
                }
                Cmsg::Fds(fds)
            } else {
                Cmsg::Raw(std::slice::from_raw_parts(body, len).to_vec())
            };
            cmsgs.push((level, ty, msg));
            c = libc::CMSG_NXTHDR(&m, c);
        }
    }
    let from = (from.len() > 0).then_some(from);
    Ok((n, from, cmsgs, m.msg_flags))
}

/// A host file's identity (device, inode).
#[cfg(unix)]
fn file_key(fd: &std::os::fd::OwnedFd) -> SysResult<(u64, u64)> {
    // SAFETY: fstat fills `st`.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(std::os::fd::AsRawFd::as_raw_fd(fd), &mut st) } < 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    #[allow(clippy::unnecessary_cast)]
    Ok((st.st_dev as u64, st.st_ino as u64))
}

/// The runtime's own objects (pipes, eventfds...) on their way to another
/// guest process: a token file's identity → the open file description.
/// The receiver trades the token for the object (`adopt`), so both share
/// it, as descriptors passed between processes do.
#[cfg(unix)]
static TOKENS: std::sync::LazyLock<Mutex<std::collections::HashMap<(u64, u64), Arc<Mutex<Open>>>>> =
    std::sync::LazyLock::new(Default::default);

#[cfg(unix)]
fn token(open: Arc<Mutex<Open>>) -> SysResult<std::os::fd::OwnedFd> {
    let fd = crate::shared::memfd(c"wasm-env-token", 0)?;
    TOKENS.lock().unwrap().insert(file_key(&fd)?, open);
    Ok(fd)
}

#[cfg(unix)]
impl Process {
    /// Control messages received on a host socket, in the guest's layout (the
    /// host's, on 64-bit Linux), with passed descriptors installed as the
    /// guest's.
    fn guest_control(&self, cmsgs: Vec<(i32, i32, Cmsg)>, cloexec: bool) -> SysResult<Vec<u8>> {
        let mut out = Vec::new();
        for (level, ty, body) in cmsgs {
            let body = match body {
                Cmsg::Fds(fds) => {
                    let mut guest = Vec::new();
                    for host in fds {
                        guest.extend_from_slice(&(self.adopt(host, cloexec)? as i32).to_le_bytes());
                    }
                    guest
                }
                Cmsg::Raw(bytes) => bytes,
            };
            out.extend_from_slice(&(16 + body.len() as u64).to_le_bytes());
            out.extend_from_slice(&level.to_le_bytes());
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&body);
            out.resize(out.len().next_multiple_of(8), 0);
        }
        Ok(out)
    }

    /// The descriptors of the SCM_RIGHTS messages in a guest control buffer,
    /// as host descriptors to pass.
    fn passed_fds(&self, data: &[u8], control: u64, len: u64) -> SysResult<Vec<std::os::fd::OwnedFd>> {
        let (mut at, end) = (control, control + len);
        let mut fds = Vec::new();
        while at + 16 <= end {
            let len = mem::read_u64(data, at)?;
            let head = mem::slice(data, at + 8, 8)?;
            let (level, ty) = (i32::from_le_bytes(head[..4].try_into().unwrap()), i32::from_le_bytes(head[4..].try_into().unwrap()));
            if len < 16 || at + len > end {
                return Err(EINVAL);
            }
            if level == SOL_SOCKET && ty == SCM_RIGHTS {
                for k in 0..(len - 16) / 4 {
                    let fd = i32::from_le_bytes(mem::slice(data, at + 16 + 4 * k, 4)?.try_into().unwrap());
                    fds.push(self.passable(fd as i64)?);
                }
            }
            at += len.next_multiple_of(8);
        }
        Ok(fds)
    }

    /// What to pass for the guest's `fd`: the host object itself (a file,
    /// a memfd, a socket, a descriptor passed in, standard input or output,
    /// a directory), or for the runtime's own objects, a token (see TOKENS).
    fn passable(&self, fd: i64) -> SysResult<std::os::fd::OwnedFd> {
        match self.host_file(fd) {
            Err(EACCES) => {}
            other => return other,
        }
        let open = self.fds.get(fd)?.open.clone();
        let stdio = match &*open.lock().unwrap() {
            Open::Stdin => 0,
            Open::Stdout => 1,
            Open::Stderr => 2,
            Open::Dir { path, .. } => {
                return std::fs::File::open(self.fs.host(path)).map(Into::into).map_err(|e| crate::fs::errno(&e));
            }
            _ => -1,
        };
        if stdio >= 0 {
            // SAFETY: the runtime's own standard descriptors stay open.
            let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(stdio) };
            return fd.try_clone_to_owned().map_err(|e| errno(&e));
        }
        token(open)
    }

    /// Installs a descriptor passed in as the guest's: the runtime's own
    /// object for a token, else by what it is on the host (a socket, a file
    /// or memfd, a directory, or anything else as `Open::Host`).
    fn adopt(&self, host: std::os::fd::OwnedFd, cloexec: bool) -> SysResult<i64> {
        use std::os::fd::AsRawFd;
        if let Some(open) = TOKENS.lock().unwrap().remove(&file_key(&host)?) {
            return self.fds.insert(Fd { open, cloexec, flags: 2 }, 0);
        }
        let raw = host.as_raw_fd();
        // SAFETY: fstat fills `st`; F_GETFL reads the flags.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(raw, &mut st) } < 0 {
            return Err(errno(&std::io::Error::last_os_error()));
        }
        let fl = unsafe { libc::fcntl(raw, libc::F_GETFL) }.max(0);
        // What the host says it is (a memfd reads as /memfd:NAME (deleted)).
        let path = || {
            let link = std::fs::read_link(format!("/proc/self/fd/{raw}")).unwrap_or_default();
            self.fs.guest(&link).unwrap_or_else(|| link.to_string_lossy().into_owned())
        };
        let open = match st.st_mode & libc::S_IFMT {
            libc::S_IFSOCK => {
                let sock = Socket::from(host);
                let kind = sock.r#type().map(|t| libc::c_int::from(t) as i64).unwrap_or(SOCK_STREAM);
                Open::Socket(Arc::new(GuestSocket::new(sock, kind)))
            }
            libc::S_IFREG => Open::File { path: path(), file: std::fs::File::from(host), append: fl & libc::O_APPEND != 0 },
            libc::S_IFDIR => Open::Dir { path: path(), entries: None, pos: 0 },
            _ => Open::Host(Arc::new(crate::fs::HostFd::new(host))),
        };
        let flags = (fl & (libc::O_ACCMODE | libc::O_NONBLOCK | libc::O_APPEND)) as i64;
        self.fds.insert(Fd { open: Arc::new(Mutex::new(open)), cloexec, flags }, 0)
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
        let fs = crate::fs::Fs::new("/".into());
        for a in ["127.0.0.1:8080", "[::1]:443", "[fe80::1%3]:53"] {
            let sa: SockAddr = a.parse::<SocketAddr>().unwrap().into();
            let bytes = sockaddr_bytes(&sa, &fs);
            let back = read_sockaddr(&bytes, 0, bytes.len() as u64).unwrap();
            assert_eq!(back.as_socket(), sa.as_socket(), "{a}");
        }
        let v4 = sockaddr_bytes(&"10.1.2.3:80".parse::<SocketAddr>().unwrap().into(), &fs);
        assert_eq!(&v4[..8], &[2, 0, 0, 80, 10, 1, 2, 3], "family (LE), port (BE), address");
    }

    #[test]
    fn unix_paths_are_the_guests() {
        let fs = crate::fs::Fs::new("/srv/root".into());
        let a = unix_sockaddr(b"/srv/root/tmp/s\0").unwrap();
        assert_eq!(sockaddr_bytes(&a, &fs), b"\x01\0/tmp/s\0");
        let abs = unix_sockaddr(b"\0name").unwrap();
        assert_eq!(sockaddr_bytes(&abs, &fs), b"\x01\0\0name", "abstract names as they are");
        assert!(unix_sockaddr(&[b'x'; 109]).is_err(), "longer than sun_path");
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
    #[cfg(unix)]
    {
        let mut n: libc::c_int = 0;
        // SAFETY: FIONREAD writes one int.
        let r = unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&s.sock), libc::FIONREAD, &mut n) };
        if r < 0 {
            return Err(errno(&std::io::Error::last_os_error()));
        }
        Ok(n.max(0) as usize)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{FIONREAD, ioctlsocket};
        let mut n: u32 = 0;
        // SAFETY: FIONREAD writes one u_long.
        let r = unsafe { ioctlsocket(std::os::windows::io::AsRawSocket::as_raw_socket(&s.sock) as usize, FIONREAD, &mut n) };
        if r != 0 {
            return Err(errno(&std::io::Error::last_os_error()));
        }
        Ok(n as usize)
    }
}

/// A host descriptor to poll: a file descriptor on Unix hosts, a socket on
/// Windows.
#[cfg(unix)]
type Pollable = std::os::fd::RawFd;
#[cfg(windows)]
type Pollable = std::os::windows::io::RawSocket;

fn pollable(s: &Socket) -> Pollable {
    #[cfg(unix)]
    return std::os::fd::AsRawFd::as_raw_fd(s);
    #[cfg(windows)]
    return std::os::windows::io::AsRawSocket::as_raw_socket(s);
}

/// Polls host descriptors for (Linux's) POLLIN and POLLOUT, waiting up to
/// `wait`: their revents, in Linux's bits.
fn host_poll(fds: &[(Pollable, i16)], wait: Duration) -> SysResult<Vec<i16>> {
    #[cfg(unix)]
    {
        let mut pfds: Vec<libc::pollfd> = fds.iter().map(|&(fd, events)| libc::pollfd { fd, events, revents: 0 }).collect();
        // SAFETY: pfds is a valid array of pollfd for its length.
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, wait.as_millis() as i32) };
        if n < 0 {
            return Err(errno(&std::io::Error::last_os_error()));
        }
        Ok(pfds.iter().map(|p| p.revents).collect())
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{POLLERR, POLLHUP, POLLNVAL, POLLRDNORM, POLLWRNORM, WSAPOLLFD, WSAPoll};
        // Winsock's bits are its own.
        let (linux_in, linux_out, linux_err, linux_hup, linux_nval) = (1i16, 4i16, 8i16, 16i16, 32i16);
        let mut pfds: Vec<WSAPOLLFD> = fds
            .iter()
            .map(|&(fd, events)| WSAPOLLFD {
                fd: fd as usize,
                events: (if events & linux_in != 0 { POLLRDNORM } else { 0 }) | (if events & linux_out != 0 { POLLWRNORM } else { 0 }),
                revents: 0,
            })
            .collect();
        // SAFETY: pfds is a valid array of WSAPOLLFD for its length.
        let n = unsafe { WSAPoll(pfds.as_mut_ptr(), pfds.len() as u32, wait.as_millis() as i32) };
        if n < 0 {
            return Err(errno(&std::io::Error::last_os_error()));
        }
        Ok(pfds
            .iter()
            .map(|p| {
                let r = p.revents;
                (if r & POLLRDNORM != 0 { linux_in } else { 0 })
                    | (if r & POLLWRNORM != 0 { linux_out } else { 0 })
                    | (if r & POLLERR != 0 { linux_err } else { 0 })
                    | (if r & POLLHUP != 0 { linux_hup } else { 0 })
                    | (if r & POLLNVAL != 0 { linux_nval } else { 0 })
            })
            .collect())
    }
}
