//! Access to the program's linear memory, and the address space the
//! runtime manages inside it for mmap (docs/abi.md, "Memory").

use std::collections::BTreeMap;

use crate::abi::{PAGE_SIZE, errno::*};

pub type SysResult<T> = Result<T, i64>;

/// Bounds-checked views of guest memory. Out-of-range pointers are EFAULT.
pub fn slice(data: &[u8], addr: u64, len: u64) -> SysResult<&[u8]> {
    let end = addr.checked_add(len).ok_or(EFAULT)?;
    data.get(addr as usize..end as usize).ok_or(EFAULT)
}

pub fn slice_mut(data: &mut [u8], addr: u64, len: u64) -> SysResult<&mut [u8]> {
    let end = addr.checked_add(len).ok_or(EFAULT)?;
    data.get_mut(addr as usize..end as usize).ok_or(EFAULT)
}

pub fn write(data: &mut [u8], addr: u64, bytes: &[u8]) -> SysResult<()> {
    slice_mut(data, addr, bytes.len() as u64)?.copy_from_slice(bytes);
    Ok(())
}

pub fn read_u64(data: &[u8], addr: u64) -> SysResult<u64> {
    Ok(u64::from_le_bytes(slice(data, addr, 8)?.try_into().unwrap()))
}

pub fn write_u64(data: &mut [u8], addr: u64, v: u64) -> SysResult<()> {
    write(data, addr, &v.to_le_bytes())
}

/// Reads a NUL-terminated string (at most PATH_MAX bytes).
pub fn cstr(data: &[u8], addr: u64) -> SysResult<String> {
    let rest = data.get(addr as usize..).ok_or(EFAULT)?;
    let len = rest.iter().take(4096).position(|&b| b == 0).ok_or(ENAMETOOLONG)?;
    // Paths are bytes on Linux; non-UTF-8 names are rare enough for now.
    String::from_utf8(rest[..len].to_vec()).map_err(|_| EINVAL)
}

/// Reads a NUL-terminated byte string (argv and envp entries, up to
/// 128 KB). These are raw bytes on Linux, not necessarily UTF-8: BusyBox,
/// for one, flags re-executed processes by setting the high bit of
/// argv[0][0].
pub fn cstr_bytes(data: &[u8], addr: u64) -> SysResult<Vec<u8>> {
    let rest = data.get(addr as usize..).ok_or(EFAULT)?;
    let len = rest.iter().take(128 << 10).position(|&b| b == 0).ok_or(E2BIG)?;
    Ok(rest[..len].to_vec())
}

/// An iovec array: (base, len) pairs.
pub fn iovecs(data: &[u8], addr: u64, count: u64) -> SysResult<Vec<(u64, u64)>> {
    if count > 1024 {
        return Err(EINVAL);
    }
    (0..count)
        .map(|i| Ok((read_u64(data, addr + i * 16)?, read_u64(data, addr + i * 16 + 8)?)))
        .collect()
}

/// Zeroes [addr, addr + len) of guest memory without touching it where it
/// can: whole pages are handed back to the host (they read as zero again,
/// and take no memory until used), the partial pages at the ends are
/// cleared. Only for anonymous memory (the mmap region and the break), not
/// the guest image, whose pages would read back as the file's.
pub fn zero(data: &mut [u8], addr: u64, len: u64) -> SysResult<()> {
    let range = slice_mut(data, addr, len)?;
    #[cfg(unix)]
    {
        let page = host_page() as usize;
        let start = range.as_ptr() as usize;
        let (first, last) = (start.next_multiple_of(page), (start + range.len()) / page * page);
        if last > first {
            // SAFETY: [first, last) lies within `range`, memory this process
            // owns; MADV_DONTNEED on private anonymous memory makes it read
            // as zero on Linux. Elsewhere (macOS's MADV_DONTNEED is only
            // advice) new anonymous pages replace them.
            #[cfg(target_os = "linux")]
            let r = unsafe { libc::madvise(first as *mut libc::c_void, last - first, libc::MADV_DONTNEED) };
            #[cfg(not(target_os = "linux"))]
            let r = unsafe { fresh_pages(first, last - first) };
            if r == 0 {
                range[..first - start].fill(0);
                range[last - start..].fill(0);
                return Ok(());
            }
        }
    }
    range.fill(0);
    Ok(())
}

/// Replaces [addr, addr + len) (page-aligned, the process's own private
/// anonymous memory) with new zero pages that take no memory until used.
/// 0 on success, as madvise.
///
/// # Safety
/// The range must be such memory, and nothing may rely on its contents.
#[cfg(unix)]
#[cfg_attr(target_os = "linux", allow(dead_code))] // (Linux uses MADV_DONTNEED.)
pub unsafe fn fresh_pages(addr: usize, len: usize) -> i32 {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED;
    // SAFETY: as the caller promises; MAP_FIXED replaces exactly the range.
    let p = unsafe { libc::mmap(addr as *mut libc::c_void, len, libc::PROT_READ | libc::PROT_WRITE, flags, -1, 0) };
    if p == libc::MAP_FAILED { -1 } else { 0 }
}

/// The host's page size: 4 KiB on x86-64, 16 KiB on Apple silicon. Host
/// mappings inside the guest's memory (the guest image, shared mappings)
/// are made of these; the guest's own page size stays PAGE_SIZE.
pub fn host_page() -> u64 {
    static PAGE: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        #[cfg(unix)]
        // SAFETY: sysconf only reads a value.
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        #[cfg(not(unix))]
        let p = 4096;
        (p as u64).max(PAGE_SIZE)
    });
    *PAGE
}

/// How far the program break may grow (the region between the heap base and
/// the mmap area). Beyond it brk fails, and allocators fall back to mmap.
pub const BRK_MAX: u64 = 64 << 20;

pub fn page_align(n: u64) -> u64 {
    n.div_ceil(PAGE_SIZE) * PAGE_SIZE
}

/// The mmap region: everything from `start` (the page-aligned heap base)
/// upward. Allocations come from a first-fit free list, else from `top`,
/// which the caller backs by growing linear memory.
#[derive(Clone)]
pub struct AddressSpace {
    pub start: u64,
    pub top: u64,
    /// Free ranges: address -> length, never adjacent (merged on free).
    free: BTreeMap<u64, u64>,
}

impl AddressSpace {
    pub fn new(heap_base: u64) -> Self {
        let start = page_align(heap_base);
        AddressSpace { start, top: start, free: BTreeMap::new() }
    }

    /// Returns the address of `len` bytes (a multiple of the page size).
    /// Reused ranges may hold old data; the caller zeroes them.
    pub fn alloc(&mut self, len: u64) -> u64 {
        if let Some((&addr, &size)) = self.free.iter().find(|&(_, &size)| size >= len) {
            self.free.remove(&addr);
            if size > len {
                self.free.insert(addr + len, size - len);
            }
            return addr;
        }
        let addr = self.top;
        self.top += len;
        addr
    }

    /// Like `alloc`, at a multiple of `align` (a power of two, a multiple
    /// of the page size): for mappings made of the host's pages, which may
    /// be larger than the guest's (16 KiB on Apple silicon).
    pub fn alloc_aligned(&mut self, len: u64, align: u64) -> u64 {
        if align <= PAGE_SIZE {
            return self.alloc(len);
        }
        let span = len + align - PAGE_SIZE;
        let addr = self.alloc(span);
        let aligned = addr.next_multiple_of(align);
        if aligned > addr {
            self.release(addr, aligned - addr);
        }
        if addr + span > aligned + len {
            self.release(aligned + len, addr + span - aligned - len);
        }
        aligned
    }

    /// Returns [addr, addr+len) to the free list, merging neighbours.
    pub fn release(&mut self, mut addr: u64, mut len: u64) {
        // Carve out any overlap with ranges that are already free.
        let overlapping: Vec<_> =
            self.free.range(..addr + len).filter(|&(&a, &l)| a + l > addr).map(|(&a, &l)| (a, l)).collect();
        for (a, l) in overlapping {
            self.free.remove(&a);
            let end = (a + l).max(addr + len);
            addr = addr.min(a);
            len = end - addr;
        }
        if let Some((&a, &l)) = self.free.range(..addr).next_back()
            && a + l == addr
        {
            self.free.remove(&a);
            addr = a;
            len += l;
        }
        if let Some(&l) = self.free.get(&(addr + len)) {
            self.free.remove(&(addr + len));
            len += l;
        }
        self.free.insert(addr, len);
    }

    /// Claims exactly [addr, addr+len) for MAP_FIXED, replacing whatever
    /// was mapped there. The region is extended if the range reaches past
    /// `top`.
    pub fn alloc_at(&mut self, addr: u64, len: u64) {
        if addr + len > self.top {
            let old_top = self.top;
            self.top = addr + len;
            self.release(old_top, self.top - old_top);
        }
        self.release(addr, len);
        let (&a, &l) = self.free.range(..=addr).next_back().expect("range was just released");
        self.free.remove(&a);
        if addr > a {
            self.free.insert(a, addr - a);
        }
        if a + l > addr + len {
            self.free.insert(addr + len, a + l - addr - len);
        }
    }

    /// The mapped ranges, in order: [start, top) less the free ones.
    pub fn used(&self) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut at = self.start;
        for (&a, &l) in &self.free {
            if a > at {
                out.push((at, a));
            }
            at = at.max(a + l);
        }
        if self.top > at {
            out.push((at, self.top));
        }
        out
    }

    /// Whether [addr, addr+len) is unmapped: free, or past `top`.
    pub fn is_free(&self, addr: u64, len: u64) -> bool {
        let Some(end) = addr.checked_add(len) else { return false };
        if addr < self.start {
            return false;
        }
        if addr >= self.top {
            return true;
        }
        // The part below `top` must lie in one free range.
        self.free.range(..=addr).next_back().is_some_and(|(&a, &l)| a + l >= end.min(self.top))
    }

    pub fn contains(&self, addr: u64, len: u64) -> bool {
        addr >= self.start && addr.checked_add(len).is_some_and(|end| end <= self.top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn fresh_pages_read_as_zero() {
        let page = PAGE_SIZE as usize;
        // SAFETY: a fresh anonymous region, unmapped below.
        let base = unsafe { libc::mmap(std::ptr::null_mut(), 3 * page, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
        assert_ne!(base, libc::MAP_FAILED);
        // SAFETY: the region just mapped.
        let data = unsafe { std::slice::from_raw_parts_mut(base.cast::<u8>(), 3 * page) };
        data.fill(7);
        // SAFETY: its middle page, whose contents nothing needs.
        assert_eq!(unsafe { fresh_pages(base as usize + page, page) }, 0);
        assert!(data[page..2 * page].iter().all(|&b| b == 0));
        assert!(data[..page].iter().chain(&data[2 * page..]).all(|&b| b == 7));
        // SAFETY: the region mapped above.
        unsafe { libc::munmap(base, 3 * page) };
    }

    #[test]
    fn zero_clears_whole_and_partial_pages() {
        let mut data = vec![7u8; 5 * PAGE_SIZE as usize];
        zero(&mut data, 100, 3 * PAGE_SIZE).unwrap();
        assert!(data[..100].iter().all(|&b| b == 7), "before the range");
        assert!(data[100..100 + 3 * PAGE_SIZE as usize].iter().all(|&b| b == 0), "the range");
        assert!(data[100 + 3 * PAGE_SIZE as usize..].iter().all(|&b| b == 7), "after it");
        assert_eq!(zero(&mut data, 4 * PAGE_SIZE, 2 * PAGE_SIZE), Err(EFAULT));
    }

    #[test]
    fn bounds_are_checked() {
        let mut data = vec![0u8; 16];
        assert_eq!(slice(&data, 8, 8).map(<[u8]>::len), Ok(8));
        assert_eq!(slice(&data, 9, 8).err(), Some(EFAULT), "past the end");
        assert_eq!(slice(&data, u64::MAX, 2).err(), Some(EFAULT), "overflowing range");
        assert_eq!(write(&mut data, 15, b"xy"), Err(EFAULT));
        write_u64(&mut data, 0, 0x1122334455667788).unwrap();
        assert_eq!(read_u64(&data, 0), Ok(0x1122334455667788));
        assert_eq!(data[0], 0x88, "little-endian");
    }

    #[test]
    fn strings() {
        let data = b"abc\0\xff\xfe\0no terminator".to_vec();
        assert_eq!(cstr(&data, 0).as_deref(), Ok("abc"));
        assert_eq!(cstr(&data, 4), Err(EINVAL), "paths must be UTF-8 for now");
        assert_eq!(cstr_bytes(&data, 4), Ok(vec![0xff, 0xfe]), "argv is bytes");
        assert_eq!(cstr(&data, 7), Err(ENAMETOOLONG), "no NUL before the end");
        assert_eq!(cstr(&data, 100), Err(EFAULT));
    }

    #[test]
    fn iovec_arrays() {
        let mut data = vec![0u8; 64];
        for (i, v) in [100u64, 5, 200, 7].iter().enumerate() {
            write_u64(&mut data, i as u64 * 8, *v).unwrap();
        }
        assert_eq!(iovecs(&data, 0, 2), Ok(vec![(100, 5), (200, 7)]));
        assert_eq!(iovecs(&data, 0, 5), Err(EFAULT), "array runs past memory");
        assert_eq!(iovecs(&data, 0, 2000), Err(EINVAL), "IOV_MAX");
    }

    #[test]
    fn page_alignment_and_contains() {
        assert_eq!((page_align(0), page_align(1), page_align(4096), page_align(4097)), (0, 4096, 4096, 8192));
        let mut s = AddressSpace::new(0x10000);
        let a = s.alloc(8192);
        assert!(s.contains(a, 8192));
        assert!(!s.contains(a, 8193), "past top");
        assert!(!s.contains(0x1000, 16), "below the mmap region");
        assert!(!s.contains(a, u64::MAX), "overflow");
    }

    #[test]
    fn used_ranges_skip_the_free_ones() {
        // (/proc/self/maps's mappings.)
        let mut s = AddressSpace::new(0x10000);
        let a = s.alloc(0x2000);
        let b = s.alloc(0x1000);
        let c = s.alloc(0x3000);
        assert_eq!(s.used(), [(a, c + 0x3000)]);
        s.release(b, 0x1000);
        assert_eq!(s.used(), [(a, a + 0x2000), (c, c + 0x3000)]);
    }

    #[test]
    fn alloc_release_reuse() {
        let mut s = AddressSpace::new(100);
        assert_eq!(s.start, 4096);
        let a = s.alloc(8192);
        let b = s.alloc(4096);
        assert_eq!((a, b), (4096, 12288));
        s.release(a, 8192);
        assert_eq!(s.alloc(4096), 4096, "first fit reuses the hole");
        s.release(4096, 4096);
        s.release(b, 4096);
        assert_eq!(s.alloc(12288), 4096, "released neighbours merge");

        s.alloc_at(40960, 4096);
        assert_eq!(s.top, 45056, "MAP_FIXED past top extends the region");
        assert_eq!(s.alloc(4096), 16384, "the gap before it is free");
    }
}
