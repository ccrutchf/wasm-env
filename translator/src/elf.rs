//! Loading the parts of an arm64 ELF executable the translator needs.

use anyhow::{Context, Result, bail};
use object::elf;
use object::read::elf::{ElfFile64, FileHeader, ProgramHeader};
use object::read::ObjectSymbolTable;
use object::{Endianness, Object, ObjectSection, ObjectSymbol, RelocationFlags, RelocationTarget, SectionFlags, SymbolKind};

pub struct Segment {
    pub vaddr: u64,
    /// Bytes from the file; the rest of `memsz` is zero (.bss).
    pub data: Vec<u8>,
    pub memsz: u64,
    pub exec: bool,
    #[allow(dead_code)]
    pub write: bool,
}

impl Segment {
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.vaddr && addr < self.vaddr + self.memsz
    }
}

pub struct Image {
    pub entry: u64,
    pub segments: Vec<Segment>,
    /// Program headers as loaded in memory (AT_PHDR), if they are.
    pub phdr: Option<u64>,
    pub phnum: u16,
    /// Function symbols, if the binary isn't stripped. Used as extra block
    /// entry points and for names in diagnostics.
    pub functions: Vec<(u64, String)>,
    /// Address ranges holding code: executable sections when the binary has
    /// section headers (so read-only data sharing an executable segment is
    /// excluded), otherwise whole executable segments.
    pub code: Vec<std::ops::Range<u64>>,
}

/// Where a position-independent executable (ET_DYN) is loaded. Bases are
/// fixed at translation time, so the 1:1 guest-address model still holds:
/// each shared object gets a base assigned once and everything is prelinked.
pub const PIE_BASE: u64 = 0x40_0000;

/// One relocation, with its site and symbol resolved to absolute addresses
/// once, then applied against the whole-image symbol table.
struct Reloc {
    at: u64,
    r_type: u32,
    /// The referenced symbol's name (for symbol-based relocations).
    sym: Option<String>,
    addend: i64,
}

/// A single ELF object loaded at a base, before cross-object relocation.
struct Parsed {
    base: u64,
    segments: Vec<Segment>,
    entry: u64,
    phdr: Option<u64>,
    phnum: u16,
    functions: Vec<(u64, String)>,
    code: Vec<std::ops::Range<u64>>,
    relocs: Vec<Reloc>,
    /// Global/weak symbols this object defines: (name, absolute address).
    defined: Vec<(String, u64)>,
    /// End of the object in memory (for placing the next one).
    top: u64,
}

/// The load base for a lone object: fixed for ET_EXEC, the PIE base for
/// ET_DYN (a PIE or shared object).
fn elf_base(bytes: &[u8]) -> Result<u64> {
    let file = ElfFile64::<Endianness>::parse(bytes).context("not a 64-bit ELF file")?;
    match file.elf_header().e_type(file.endian()) {
        elf::ET_EXEC => Ok(0),
        elf::ET_DYN => Ok(PIE_BASE),
        other => bail!("unsupported ELF type {other}"),
    }
}

/// Parses one ELF object and places its segments at `base`. Relocations and
/// exported symbols are recorded but not yet applied (that needs the whole
/// image's symbol table).
fn parse_object(bytes: &[u8], base: u64) -> Result<Parsed> {
    let file = ElfFile64::<Endianness>::parse(bytes).context("not a 64-bit ELF file")?;
    let header = file.elf_header();
    let endian = file.endian();
    if header.e_machine(endian) != elf::EM_AARCH64 {
        bail!("not an arm64 binary (e_machine {})", header.e_machine(endian));
    }
    match header.e_type(endian) {
        elf::ET_EXEC | elf::ET_DYN => {}
        other => bail!("unsupported ELF type {other}"),
    }
    let phoff = header.e_phoff(endian);
    let mut segments = Vec::new();
    let mut phdr = None;
    for ph in file.elf_program_headers() {
        if ph.p_type(endian) == elf::PT_LOAD {
            let offset = ph.p_offset(endian);
            let filesz = ph.p_filesz(endian);
            let vaddr = base + ph.p_vaddr(endian);
            if phoff >= offset && phoff < offset + filesz {
                phdr = Some(vaddr + (phoff - offset));
            }
            let flags = ph.p_flags(endian);
            segments.push(Segment {
                vaddr,
                data: ph.data(endian, bytes).map_err(|_| anyhow::anyhow!("bad segment"))?.to_vec(),
                memsz: ph.p_memsz(endian),
                exec: flags & elf::PF_X != 0,
                write: flags & elf::PF_W != 0,
            });
        }
    }
    // PT_INTERP (a dynamic executable) is fine: we do the loading ourselves,
    // given the libraries, so we ignore the requested interpreter.

    let mut relocs = Vec::new();
    if let Some(dyn_relocs) = file.dynamic_relocations() {
        let syms = file.dynamic_symbol_table();
        for (offset, r) in dyn_relocs {
            let RelocationFlags::Elf { r_type } = r.flags() else { continue };
            let sym = match r.target() {
                RelocationTarget::Symbol(idx) => {
                    let table = syms.ok_or_else(|| anyhow::anyhow!("relocation symbol without a symbol table"))?;
                    let s = table.symbol_by_index(idx).map_err(|_| anyhow::anyhow!("bad symbol index"))?;
                    // A defined local symbol resolves within this object; an
                    // undefined one is looked up image-wide by name.
                    if s.is_undefined() { Some(s.name().unwrap_or("").to_string()) } else { None }
                }
                _ => None,
            };
            // For a defined-symbol relocation, fold the symbol's own address
            // into the addend now (base applied at apply time).
            let addend = match r.target() {
                RelocationTarget::Symbol(idx) if sym.is_none() => {
                    let table = syms.unwrap();
                    let s = table.symbol_by_index(idx).unwrap();
                    r.addend() + s.address() as i64
                }
                _ => r.addend(),
            };
            relocs.push(Reloc { at: base + offset, r_type, sym, addend });
        }
    }

    let defined = file
        .dynamic_symbols()
        .filter(|s| !s.is_undefined() && s.name().is_ok_and(|n| !n.is_empty()))
        .map(|s| (s.name().unwrap().to_string(), base + s.address()))
        .collect();

    let mut functions: Vec<(u64, String)> = file
        .symbols()
        .filter(|s| s.kind() == SymbolKind::Text && s.address() != 0)
        .map(|s| (base + s.address(), s.name().unwrap_or("?").to_string()))
        .collect();
    functions.sort();
    functions.dedup_by_key(|f| f.0);

    let mut code: Vec<_> = file
        .sections()
        .filter(|s| matches!(s.flags(), SectionFlags::Elf { sh_flags } if sh_flags & elf::SHF_EXECINSTR as u64 != 0))
        .filter(|s| s.size() > 0)
        .map(|s| base + s.address()..base + s.address() + s.size())
        .collect();
    if code.is_empty() {
        code = segments.iter().filter(|s| s.exec).map(|s| s.vaddr..s.vaddr + s.data.len() as u64).collect();
    }
    code.sort_by_key(|r| r.start);

    let top = segments.iter().map(|s| s.vaddr + s.memsz).max().unwrap_or(base);
    Ok(Parsed {
        base,
        segments,
        entry: base + header.e_entry(endian),
        phdr,
        phnum: header.e_phnum(endian),
        functions,
        code,
        relocs,
        defined,
        top,
    })
}

/// Prelinks parsed objects into one image: builds the image-wide symbol
/// table, applies every relocation to final values, and merges segments.
/// The first object is the executable (its entry and program headers win).
fn link(objects: Vec<Parsed>) -> Result<Image> {
    // arm64 dynamic relocation types.
    const RELATIVE: u32 = 1027;
    const GLOB_DAT: u32 = 1025;
    const JUMP_SLOT: u32 = 1026;
    const ABS64: u32 = 257;

    let mut global = std::collections::HashMap::new();
    for obj in &objects {
        for (name, addr) in &obj.defined {
            global.entry(name.clone()).or_insert(*addr);
        }
    }

    let exe = &objects[0];
    let (entry, phdr, phnum) = (exe.entry, exe.phdr, exe.phnum);
    let mut segments: Vec<Segment> = Vec::new();
    let mut functions = Vec::new();
    let mut code = Vec::new();

    for mut obj in objects {
        for r in &obj.relocs {
            let value = match r.r_type {
                RELATIVE => obj.base.wrapping_add(r.addend as u64),
                GLOB_DAT | JUMP_SLOT | ABS64 => match &r.sym {
                    // A local defined symbol was folded into the addend.
                    None => obj.base.wrapping_add(r.addend as u64),
                    Some(name) => {
                        let addr = global
                            .get(name)
                            .copied()
                            .ok_or_else(|| anyhow::anyhow!("undefined symbol `{name}` (provide the library with --lib)"))?;
                        addr.wrapping_add(r.addend as u64)
                    }
                },
                other => bail!("unsupported relocation type {other}"),
            };
            write_word(&mut obj.segments, r.at, value)?;
        }
        segments.append(&mut obj.segments);
        functions.append(&mut obj.functions);
        code.append(&mut obj.code);
    }
    code.sort_by_key(|r| r.start);
    functions.sort();
    functions.dedup_by_key(|f| f.0);
    Ok(Image { entry, segments, phdr, phnum, functions, code })
}

/// Writes an 8-byte little-endian value at a guest address, growing the
/// owning segment's file data into .bss if needed.
fn write_word(segments: &mut [Segment], addr: u64, value: u64) -> Result<()> {
    let seg = segments
        .iter_mut()
        .find(|s| addr >= s.vaddr && addr + 8 <= s.vaddr + s.memsz)
        .ok_or_else(|| anyhow::anyhow!("relocation at {addr:#x} outside any segment"))?;
    let off = (addr - seg.vaddr) as usize;
    if off + 8 > seg.data.len() {
        seg.data.resize(off + 8, 0);
    }
    seg.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

impl Image {
    /// Loads a single statically-linked or position-independent executable.
    pub fn load(bytes: &[u8]) -> Result<Image> {
        let base = elf_base(bytes)?;
        link(vec![parse_object(bytes, base)?])
    }

    /// Loads a dynamically-linked executable together with the shared
    /// objects it needs, prelinking them into one image. `libs` are
    /// (name, bytes) pairs; symbols are resolved across all of them.
    pub fn load_dynamic(exe: &[u8], libs: &[(String, Vec<u8>)]) -> Result<Image> {
        let mut objects = vec![parse_object(exe, elf_base(exe)?)?];
        // Place each library above the previous object, page-aligned.
        let mut next = objects[0].top.next_multiple_of(0x10000).max(PIE_BASE);
        for (name, bytes) in libs {
            let obj = parse_object(bytes, next).with_context(|| format!("loading {name}"))?;
            next = obj.top.next_multiple_of(0x10000);
            objects.push(obj);
        }
        link(objects)
    }

    /// Decodes every 4-byte-aligned word of code (see `code`). arm64
    /// instructions are fixed-width, so this finds every instruction; words
    /// that don't decode are data (literal pools) or unsupported encodings.
    pub fn decode_all(&self) -> Vec<(u64, u32, Option<bad64::Instruction>)> {
        let mut out = Vec::new();
        for range in &self.code {
            let Some(seg) = self.segments.iter().find(|s| s.exec && s.contains(range.start)) else { continue };
            let mut addr = range.start.next_multiple_of(4);
            while addr + 4 <= range.end {
                let off = (addr - seg.vaddr) as usize;
                let Some(bytes) = seg.data.get(off..off + 4) else { break };
                let word = u32::from_le_bytes(bytes.try_into().unwrap());
                out.push((addr, word, bad64::decode(word, addr).ok()));
                addr += 4;
            }
        }
        out
    }

    /// Reads a little-endian unsigned integer of `width` bytes at a guest
    /// virtual address, if it lies within a loaded segment.
    pub fn read_uint(&self, vaddr: u64, width: u32) -> Option<u64> {
        let seg = self.segments.iter().find(|s| s.contains(vaddr) && s.contains(vaddr + width as u64 - 1))?;
        let off = (vaddr - seg.vaddr) as usize;
        let bytes = seg.data.get(off..off + width as usize)?;
        let mut v = 0u64;
        for (k, b) in bytes.iter().enumerate() {
            v |= (*b as u64) << (8 * k);
        }
        Some(v)
    }

    /// Whether `addr` is a 4-byte-aligned address inside executable code.
    pub fn is_code(&self, addr: u64) -> bool {
        addr.is_multiple_of(4) && self.code.iter().any(|r| r.contains(&addr))
    }

    pub fn function_at(&self, addr: u64) -> Option<&str> {
        let i = self.functions.partition_point(|f| f.0 <= addr);
        i.checked_sub(1).map(|i| self.functions[i].1.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn loads_a_minimal_static_binary() {
        let Some(bytes) = testutil::min() else { return };
        let image = Image::load(&bytes).unwrap();
        let start = image.functions.iter().find(|f| f.1 == "_start").expect("_start symbol").0;
        assert_eq!(image.entry, start);
        let code: Vec<_> = image.segments.iter().filter(|s| s.exec).collect();
        assert_eq!(code.len(), 1);
        assert!(code[0].contains(image.entry));
        assert_eq!(code[0].data.len() % 4, 0);
        assert_eq!(image.function_at(image.entry), Some("_start"));
    }

    #[test]
    fn loads_a_static_libc_binary() {
        let Some(bytes) = testutil::static_c("c/hello.c") else { return };
        let image = Image::load(&bytes).unwrap();
        let data = image.segments.iter().find(|s| s.write).expect("a writable segment");
        assert!(!data.exec);
        assert!(data.memsz > data.data.len() as u64, ".bss follows the file data");
        let phdr = image.phdr.expect("program headers are loaded");
        assert!(image.segments.iter().any(|s| s.contains(phdr)), "AT_PHDR points into the image");
        assert!(image.phnum >= 2);
        assert!(image.functions.iter().any(|f| f.1 == "main"));
    }

    #[test]
    fn rejects_what_it_cant_translate() {
        let err = |bytes: &[u8]| Image::load(bytes).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(b"not an elf").contains("not a 64-bit ELF"));
        // The test binary itself: an ELF for the host, not arm64.
        let host = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        if cfg!(target_arch = "aarch64") {
            return;
        }
        assert!(err(&host).contains("not an arm64 binary"), "got: {}", err(&host));
        if let Some(dynamic) = testutil::fixture("c/hello.c", &["-O2", "-no-pie"]) {
            let e = err(&dynamic);
            assert!(e.contains("undefined symbol") || e.contains("dynamically") || e.contains("statically"), "got: {e}");
        }
    }

    #[test]
    fn code_excludes_read_only_data_in_the_executable_segment() {
        let Some(bytes) = testutil::min() else { return };
        let image = Image::load(&bytes).unwrap();
        let exec = image.segments.iter().find(|s| s.exec).unwrap();
        let code: u64 = image.code.iter().map(|r| r.end - r.start).sum();
        assert!(code < exec.data.len() as u64, "strings share the segment but aren't code");
        assert!(image.code.iter().any(|r| r.contains(&image.entry)));
    }

    #[test]
    fn decodes_every_word_of_code() {
        let Some(bytes) = testutil::min() else { return };
        let image = Image::load(&bytes).unwrap();
        let words: usize = image.code.iter().map(|r| ((r.end - r.start) / 4) as usize).sum();
        let decoded = image.decode_all();
        assert_eq!(decoded.len(), words);
        assert!(decoded.windows(2).all(|w| w[1].0 == w[0].0 + 4), "consecutive addresses");
        let (_, word, insn) = decoded.iter().find(|d| d.0 == image.entry).unwrap();
        assert_eq!(insn.as_ref().unwrap().opcode(), *word);
    }
}

#[cfg(test)]
mod pie_tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn static_pie_is_loaded_at_the_pie_base_and_relocated() {
        let Some(bytes) = testutil::fixture("c/hello.c", &["-O2", "-static-pie"]) else { return };
        let image = Image::load(&bytes).unwrap();
        // Segments and entry are placed at the PIE base.
        assert!(image.entry >= PIE_BASE, "entry {:#x} below the PIE base", image.entry);
        assert!(image.segments.iter().all(|s| s.vaddr >= PIE_BASE));
        assert!(image.functions.iter().any(|f| f.1 == "main" && f.0 >= PIE_BASE));

        // A RELATIVE relocation stores an absolute pointer; after prelinking
        // every such stored code/data pointer must land inside the loaded
        // image (base applied), not near zero (the un-relocated value).
        let top = image.segments.iter().map(|s| s.vaddr + s.memsz).max().unwrap();
        let mut relocated = 0;
        for seg in image.segments.iter().filter(|s| s.write) {
            for w in seg.data.chunks_exact(8) {
                let v = u64::from_le_bytes(w.try_into().unwrap());
                if (PIE_BASE..top).contains(&v) {
                    relocated += 1;
                }
            }
        }
        assert!(relocated > 0, "expected some relocated pointers into the image");
    }

    #[test]
    fn dynamically_linked_binaries_are_rejected_for_now() {
        let Some(bytes) = testutil::fixture("c/hello.c", &["-O2"]) else { return };
        // Default musl link is dynamic (has a PT_INTERP).
        let err = Image::load(&bytes).map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("undefined symbol") || err.contains("interpreter"), "got: {err}");
    }
}

#[cfg(test)]
mod dynamic_tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn cross_object_symbols_are_prelinked() {
        // A main dynamically linked against a custom shared library.
        let Some(lib) = testutil::fixture("arm64/dynlib.c", &["-O2", "-shared", "-nostdlib", "-fno-stack-protector", "-fPIC"])
        else {
            return;
        };
        let Some(main) = testutil::fixture_linked(
            "arm64/dynmain.c",
            &["-O2", "-fPIE", "-pie", "-nostdlib", "-fno-stack-protector"],
            &["arm64/dynlib.c"],
        ) else {
            return;
        };
        // Loading main alone leaves lib_add/lib_sum/lib_value undefined.
        let err = Image::load(&main).map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("undefined symbol"), "got: {err}");

        // With the library, everything resolves and the two objects occupy
        // distinct, non-overlapping address ranges.
        let image = Image::load_dynamic(&main, &[("dynlib.so".into(), lib)]).unwrap();
        let ranges: Vec<_> = image.segments.iter().map(|s| (s.vaddr, s.vaddr + s.memsz)).collect();
        for (i, a) in ranges.iter().enumerate() {
            for b in &ranges[i + 1..] {
                assert!(a.1 <= b.0 || b.1 <= a.0, "segments {a:?} and {b:?} overlap");
            }
        }
        // main's entry is the executable's, at the PIE base.
        assert!(image.entry >= PIE_BASE);
        // The library's exported functions are present as blocks to lift.
        assert!(image.functions.iter().any(|f| f.1 == "lib_add"));
        assert!(image.functions.iter().any(|f| f.1 == "lib_sum"));
    }
}
