//! A translated program's guest image: its initial memory contents, which
//! the translator stores in the module as a custom section of whole pages
//! at page-aligned file offsets (translator/src/link.rs, `add_image`).
//!
//! The program's memory is shared between its threads, and wasmtime only
//! maps a module's data copy-on-write into memories the module defines, so
//! this does the same for the shared memory: each run of pages is mapped
//! privately from the .wasm file, and a process pays only for the pages it
//! touches (or writes).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, bail};
use wasmtime::SharedMemory;

const SECTION: &[u8] = b"wasm-env.image";

/// A run of pages: guest address, length and offset in the file.
#[derive(Debug, PartialEq)]
pub struct Run {
    pub addr: u64,
    pub len: u64,
    pub offset: u64,
}

fn uleb(f: &mut File) -> Result<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let mut b = [0u8];
        f.read_exact(&mut b)?;
        v |= u64::from(b[0] & 0x7f) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            bail!("bad LEB128");
        }
    }
}

/// The runs in a module's image section (none if it has no image).
pub fn runs(wasm: &Path) -> Result<Vec<Run>> {
    let mut f = File::open(wasm).with_context(|| wasm.display().to_string())?;
    let len = f.metadata()?.len();
    let mut pos = 8;
    while pos < len {
        f.seek(SeekFrom::Start(pos))?;
        let mut id = [0u8];
        f.read_exact(&mut id)?;
        let size = uleb(&mut f)?;
        let body = f.stream_position()?;
        pos = body + size;
        if id[0] != 0 {
            continue;
        }
        let name_len = uleb(&mut f)?;
        if name_len != SECTION.len() as u64 {
            continue;
        }
        let mut name = vec![0u8; SECTION.len()];
        f.read_exact(&mut name)?;
        if name != SECTION {
            continue;
        }
        let mut word = || -> Result<u64> {
            let mut b = [0u8; 8];
            f.read_exact(&mut b)?;
            Ok(u64::from_le_bytes(b))
        };
        let count = word()?;
        let mut runs = Vec::new();
        for _ in 0..count {
            let (addr, len, offset) = (word()?, word()?, word()?);
            if offset % 4096 != 0 || addr % 4096 != 0 || offset + len > pos {
                bail!("{}: malformed image section", wasm.display());
            }
            runs.push(Run { addr, len, offset });
        }
        return Ok(runs);
    }
    Ok(Vec::new())
}

/// Maps the image of the translated program in `wasm` into `memory`.
pub fn map(memory: &SharedMemory, wasm: &Path) -> Result<()> {
    let runs = runs(wasm)?;
    let end = runs.iter().map(|r| r.addr + r.len).max().unwrap_or(0);
    let size = memory.data_size() as u64;
    if end > size {
        memory.grow((end - size).div_ceil(65536))?;
    }
    let file = File::open(wasm)?;
    let base = memory.data().as_ptr() as *mut u8;
    for r in &runs {
        map_run(&file, base, r)?;
    }
    Ok(())
}

#[cfg(unix)]
fn map_run(file: &File, base: *mut u8, r: &Run) -> Result<()> {
    use std::os::fd::AsRawFd;
    // The file's pages can be mapped where the run is whole host pages at
    // the same offset within a host page as in the file (with 4 KiB host
    // pages, every run); the rest is copied.
    let page = crate::memory::host_page();
    let (first, last) = (r.addr.next_multiple_of(page), (r.addr + r.len) / page * page);
    if r.addr % page != r.offset % page || last <= first {
        return copy_run(file, base, r.addr, r.offset, r.len);
    }
    copy_run(file, base, r.addr, r.offset, first - r.addr)?;
    // SAFETY: [first, last) lies inside the memory (grown to cover it
    // above), which nothing else is using yet; the mapping replaces those
    // pages with a private copy-on-write view of the file.
    let p = unsafe {
        libc::mmap(
            base.add(first as usize).cast(),
            (last - first) as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_FIXED,
            file.as_raw_fd(),
            (r.offset + (first - r.addr)) as libc::off_t,
        )
    };
    if p == libc::MAP_FAILED {
        bail!("mapping the guest image: {}", std::io::Error::last_os_error());
    }
    copy_run(file, base, last, r.offset + (last - r.addr), r.addr + r.len - last)
}

#[cfg(not(unix))]
fn map_run(file: &File, base: *mut u8, r: &Run) -> Result<()> {
    copy_run(file, base, r.addr, r.offset, r.len)
}

/// Copies [offset, offset + len) of the file to guest address `addr`.
fn copy_run(file: &File, base: *mut u8, addr: u64, offset: u64, len: u64) -> Result<()> {
    // SAFETY: as for map_run: the range is inside the memory, unused yet.
    let dst = unsafe { std::slice::from_raw_parts_mut(base.add(addr as usize), len as usize) };
    let mut done = 0;
    while done < dst.len() {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(file, &mut dst[done..], offset + done as u64)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(file, &mut dst[done..], offset + done as u64)?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module with one unrelated custom section, then an image of two
    /// runs, laid out as the translator writes it.
    fn module() -> Vec<u8> {
        let mut m = b"\0asm\x01\0\0\0".to_vec();
        m.extend([0, 4, 3, b'f', b'o', b'o']);
        let start = m.len();
        let mut body = vec![SECTION.len() as u8];
        body.extend(SECTION);
        body.extend(2u64.to_le_bytes());
        for v in [0x1000u64, 0x1000, 0x2000, 0x5000, 0x1000, 0x3000] {
            body.extend(v.to_le_bytes());
        }
        body.resize(0x2000 - start - 6, 0);
        body.extend([b'a'; 0x1000]);
        body.extend([b'b'; 0x1000]);
        m.push(0);
        let size = body.len() as u64;
        for i in 0..5 {
            let byte = (size >> (7 * i)) as u8 & 0x7f;
            m.push(if i < 4 { byte | 0x80 } else { byte });
        }
        m.extend(body);
        m
    }

    #[test]
    fn finds_the_runs() {
        let path = std::env::temp_dir().join(format!("wasm-env-image-{}.wasm", std::process::id()));
        std::fs::write(&path, module()).unwrap();
        let runs = runs(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            runs.unwrap(),
            [Run { addr: 0x1000, len: 0x1000, offset: 0x2000 }, Run { addr: 0x5000, len: 0x1000, offset: 0x3000 }]
        );
    }

    #[test]
    fn a_module_without_an_image_has_no_runs() {
        let path = std::env::temp_dir().join(format!("wasm-env-noimage-{}.wasm", std::process::id()));
        std::fs::write(&path, b"\0asm\x01\0\0\0\0\x04\x03foo").unwrap();
        let runs = runs(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(runs.unwrap(), []);
    }
}
