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

pub fn page_align(n: u64) -> u64 {
    n.div_ceil(PAGE_SIZE) * PAGE_SIZE
}

/// The mmap region: everything from `start` (the page-aligned heap base)
/// upward. Allocations come from a first-fit free list, else from `top`,
/// which the caller backs by growing linear memory.
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

    pub fn contains(&self, addr: u64, len: u64) -> bool {
        addr >= self.start && addr.checked_add(len).is_some_and(|end| end <= self.top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
