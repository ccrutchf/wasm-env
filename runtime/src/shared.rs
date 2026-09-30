//! Shared mappings (docs/abi.md, "Memory"): `mmap(MAP_SHARED)` of a file,
//! a memfd or anonymous memory is a real host mapping of it, placed inside
//! the process's memory reservation at the guest address, so the guest's
//! writes reach the file and other processes (another guest process, a
//! compositor on the host) see them, and theirs the guest.
//!
//! The rest of a process's memory is the host's private anonymous memory
//! wasmtime made; unmapping a shared range puts that back, zeroed. A forked
//! child maps the same files at the same addresses.

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::sync::Arc;

#[cfg(target_os = "linux")]
use crate::abi::errno::*;
#[cfg(target_os = "linux")]
use crate::memory::SysResult;

/// A shared mapping of [start, end): `file` from `offset`.
#[derive(Clone)]
pub struct SharedMap {
    pub end: u64,
    pub file: Arc<OwnedFd>,
    pub offset: u64,
    pub writable: bool,
}

/// A process's shared mappings, by start.
pub type SharedMaps = std::collections::BTreeMap<u64, SharedMap>;

/// Maps `file` from `offset` at guest [addr, addr + len) of the memory whose
/// guest address 0 is at `base`.
#[cfg(target_os = "linux")]
pub fn map(base: *mut u8, addr: u64, len: u64, file: &OwnedFd, offset: u64, writable: bool) -> SysResult<()> {
    let prot = libc::PROT_READ | if writable { libc::PROT_WRITE } else { 0 };
    // SAFETY: [addr, addr + len) is inside the process's memory (the caller
    // grew it to cover the range), which the guest owns; MAP_FIXED replaces
    // those pages and nothing else.
    let p = unsafe {
        libc::mmap(base.add(addr as usize).cast(), len as usize, prot, libc::MAP_SHARED | libc::MAP_FIXED, file.as_raw_fd(), offset as libc::off_t)
    };
    if p == libc::MAP_FAILED {
        return Err(crate::fs::errno(&std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Puts private anonymous memory (zeroed) back over guest [addr, addr + len).
#[cfg(target_os = "linux")]
pub fn unmap(base: *mut u8, addr: u64, len: u64) -> SysResult<()> {
    // SAFETY: as in `map`; the range becomes what the rest of the memory is.
    let p = unsafe {
        libc::mmap(
            base.add(addr as usize).cast(),
            len as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(ENOMEM);
    }
    Ok(())
}

/// Removes [start, end) from `maps`, keeping the parts of mappings outside
/// it. Returns whether any overlapped (the caller then unmaps the range).
pub fn remove(maps: &mut SharedMaps, start: u64, end: u64) -> bool {
    let overlapping: Vec<u64> = maps.range(..end).filter(|(_, m)| m.end > start).map(|(&s, _)| s).collect();
    for s in &overlapping {
        let m = maps.remove(s).unwrap();
        if *s < start {
            maps.insert(*s, SharedMap { end: start, ..m.clone() });
        }
        if m.end > end {
            maps.insert(end, SharedMap { offset: m.offset + (end - s), ..m });
        }
    }
    !overlapping.is_empty()
}

/// Anonymous shared memory (`MAP_SHARED | MAP_ANONYMOUS`, shared with forked
/// children): a memfd of `len` bytes.
#[cfg(target_os = "linux")]
pub fn anonymous(len: u64) -> SysResult<OwnedFd> {
    let fd = memfd(c"wasm-env-shared", 0)?;
    // SAFETY: a descriptor memfd_create just made.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } < 0 {
        return Err(crate::fs::errno(&std::io::Error::last_os_error()));
    }
    Ok(fd)
}

/// A host memfd (always close-on-exec on the host), with the guest's flags
/// otherwise (MFD_ALLOW_SEALING).
#[cfg(target_os = "linux")]
pub fn memfd(name: &std::ffi::CStr, flags: u32) -> SysResult<OwnedFd> {
    // SAFETY: `name` is a C string; the result is a new descriptor or -1.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), (flags & libc::MFD_ALLOW_SEALING) | libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(crate::fs::errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: a new descriptor, owned from here on.
    Ok(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn removing_splits_mappings() {
        let file = Arc::new(anonymous(4 << 12).unwrap());
        let mut maps = SharedMaps::new();
        maps.insert(0x10000, SharedMap { end: 0x14000, file, offset: 0, writable: true });
        assert!(remove(&mut maps, 0x11000, 0x12000));
        let parts: Vec<(u64, u64, u64)> = maps.iter().map(|(&s, m)| (s, m.end, m.offset)).collect();
        assert_eq!(parts, [(0x10000, 0x11000, 0), (0x12000, 0x14000, 0x2000)]);
        assert!(!remove(&mut maps, 0x20000, 0x21000));
    }

    #[test]
    fn a_mapping_shares_the_file() {
        // A private anonymous region standing in for the reservation.
        let len = 4 << 12;
        // SAFETY: a fresh anonymous mapping, unmapped below.
        let base = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
        assert_ne!(base, libc::MAP_FAILED);
        let base = base.cast::<u8>();
        let file = anonymous(8192).unwrap();
        map(base, 4096, 8192, &file, 0, true).unwrap();
        // SAFETY: inside the mapping just made.
        unsafe { *base.add(4096 + 5) = 42 };
        let mut byte = [0u8];
        // SAFETY: reads one byte of the memfd.
        assert_eq!(unsafe { libc::pread(file.as_raw_fd(), byte.as_mut_ptr().cast(), 1, 5) }, 1);
        assert_eq!(byte[0], 42, "the write reached the file");
        unmap(base, 4096, 8192).unwrap();
        // SAFETY: private memory again, zeroed.
        assert_eq!(unsafe { *base.add(4096 + 5) }, 0);
        // SAFETY: the region mapped above.
        unsafe { libc::munmap(base.cast(), len) };
    }
}
