//! Loading the parts of an arm64 ELF executable the translator needs.

use std::collections::HashMap;

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
    /// The executable's entry point (AT_ENTRY). Differs from `entry` when a
    /// dynamic linker runs first.
    pub app_entry: u64,
    /// Where the dynamic linker is loaded (AT_BASE), or 0 if none runs.
    pub interp_base: u64,
    /// Extra block entry points: the app entry and every object's
    /// DT_INIT/DT_FINI, which the dynamic linker calls through addresses it
    /// computes at run time (so no stored pointer reveals them).
    pub roots: Vec<u64>,
    /// Libraries the dynamic linker will map at run time: (the path it will
    /// open, prelinked base, end). The support code places them here.
    pub libs: Vec<(String, u64, u64)>,
    /// Every object in the image (name, base, end), for naming addresses in
    /// error messages.
    pub objects: Vec<(String, u64, u64)>,
}

/// Where a position-independent executable (ET_DYN) is loaded. Bases are
/// fixed at translation time, so the 1:1 guest-address model still holds:
/// each shared object gets a base assigned once and everything is prelinked.
pub const PIE_BASE: u64 = 0x40_0000;

/// Alignment of library bases: glibc's 64 KB-aligned reservations, and a
/// multiple of the lifter's chunk span, so a library's chunks don't depend
/// on its neighbours.
const LIB_ALIGN: u64 = 0x10000;

/// The address space a shared object's PT_LOAD segments span from base 0.
fn extent(bytes: &[u8]) -> Result<u64> {
    let file = ElfFile64::<Endianness>::parse(bytes).context("not a 64-bit ELF file")?;
    let endian = file.endian();
    Ok(file
        .elf_program_headers()
        .iter()
        .filter(|ph| ph.p_type(endian) == elf::PT_LOAD)
        .map(|ph| ph.p_vaddr(endian) + ph.p_memsz(endian))
        .max()
        .unwrap_or(0))
}

/// Chooses a base for each (name, extent), at or above `start`: the base in
/// `layout` if the object still fits there, else above everything placed.
/// Keeping earlier bases keeps a library's translated code identical (and
/// cached) when libraries are added or change size.
pub fn place(objects: &[(&str, u64)], start: u64, layout: &HashMap<String, u64>) -> Vec<u64> {
    let mut bases = vec![None; objects.len()];
    let mut taken: Vec<(u64, u64)> = Vec::new();
    for (k, (name, size)) in objects.iter().enumerate() {
        if let Some(&base) = layout.get(*name) {
            let end = base + size;
            if base >= start && base % LIB_ALIGN == 0 && taken.iter().all(|&(b, e)| end <= b || base >= e) {
                taken.push((base, end.next_multiple_of(LIB_ALIGN)));
                bases[k] = Some(base);
            }
        }
    }
    let mut next = taken.iter().map(|&(_, e)| e).max().unwrap_or(start).max(start);
    bases
        .into_iter()
        .zip(objects)
        .map(|(base, (_, size))| {
            base.unwrap_or_else(|| {
                let b = next;
                next = (b + size).next_multiple_of(LIB_ALIGN);
                b
            })
        })
        .collect()
}

/// One relocation, with its site and symbol resolved to absolute addresses
/// once, then applied against the whole-image symbol table.
struct Reloc {
    at: u64,
    r_type: u32,
    /// The referenced symbol (name, is_weak) for symbol-based relocations.
    sym: Option<(String, bool)>,
    addend: i64,
    /// A RELR relocation: used only to find code pointers. The dynamic
    /// linker (or a static PIE's own startup code) applies RELR as
    /// `*p += base`, which isn't idempotent, so the image keeps the file's
    /// value for it to relocate at run time.
    discovery_only: bool,
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
    /// DT_INIT and DT_FINI, as absolute addresses.
    roots: Vec<u64>,
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
    // (vaddr, file offset, file size) of each PT_LOAD, to find tables by address.
    let mut loads = Vec::new();
    for ph in file.elf_program_headers() {
        if ph.p_type(endian) == elf::PT_LOAD {
            loads.push((ph.p_vaddr(endian), ph.p_offset(endian), ph.p_filesz(endian)));
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
    // DT_INIT and DT_FINI from the dynamic section, and the RELR table.
    let mut roots = Vec::new();
    let (mut relr, mut relr_size) = (0u64, 0u64);
    for ph in file.elf_program_headers() {
        if ph.p_type(endian) != elf::PT_DYNAMIC {
            continue;
        }
        let data = ph.data(endian, bytes).map_err(|_| anyhow::anyhow!("bad dynamic segment"))?;
        for entry in data.as_chunks::<16>().0 {
            let tag = u64::from_le_bytes(entry[..8].try_into().unwrap());
            let val = u64::from_le_bytes(entry[8..].try_into().unwrap());
            match tag as u32 {
                elf::DT_NULL => break,
                elf::DT_INIT | elf::DT_FINI if val != 0 => roots.push(base + val),
                36 => relr = val,      // DT_RELR
                35 => relr_size = val, // DT_RELRSZ
                _ => {}
            }
        }
    }
    // PT_INTERP (a dynamic executable) is fine: the caller supplies the
    // libraries and, optionally, the interpreter (see Image::load_dynamic).

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
                    if s.is_undefined() { Some((s.name().unwrap_or("").to_string(), s.is_weak())) } else { None }
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
            relocs.push(Reloc { at: base + offset, r_type, sym, addend, discovery_only: false });
            // Lazy binding: until first called, a PLT slot holds the address of
            // the PLT header (link-time value, relocated by the dynamic linker).
            if r_type == 1026
                && let Some(off) = loads.iter().find(|&&(va, _, sz)| offset >= va && offset + 8 <= va + sz).map(|&(va, fo, _)| (fo + offset - va) as usize)
                && let Some(w) = bytes.get(off..off + 8)
            {
                roots.push(base + u64::from_le_bytes(w.try_into().unwrap()));
            }
        }
    }
    // RELR: a compact list of relative relocations (entries are an address,
    // or a bitmap of the 63 words after the last one), where the stored word
    // is the link-time value and gets the base added.
    let at_file = |vaddr: u64| {
        loads.iter().find(|&&(va, _, sz)| vaddr >= va && vaddr + 8 <= va + sz).map(|&(va, off, _)| (off + vaddr - va) as usize)
    };
    let word = |off: usize| bytes.get(off..off + 8).map(|w| u64::from_le_bytes(w.try_into().unwrap()));
    if relr_size > 0 {
        let table = at_file(relr).context("RELR table outside the file")?;
        let mut next = 0u64;
        let mut relative = |addr: u64| {
            if let Some(v) = at_file(addr).and_then(word) {
                relocs.push(Reloc { at: base + addr, r_type: 1027, sym: None, addend: v as i64, discovery_only: true });
            }
        };
        for k in 0..relr_size as usize / 8 {
            let entry = word(table + 8 * k).context("truncated RELR table")?;
            if entry & 1 == 0 {
                relative(entry);
                next = entry + 8;
            } else {
                for bit in 0..63 {
                    if entry >> (bit + 1) & 1 != 0 {
                        relative(next + 8 * bit);
                    }
                }
                next += 8 * 63;
            }
        }
    }

    let defined = file
        .dynamic_symbols()
        .filter(|s| !s.is_undefined() && s.name().is_ok_and(|n| !n.is_empty()))
        .map(|s| (s.name().unwrap().to_string(), base + s.address()))
        .collect();

    // Function symbols from .symtab, and .dynsym for stripped libraries.
    let mut functions: Vec<(u64, String)> = file
        .symbols()
        .chain(file.dynamic_symbols())
        .filter(|s| s.kind() == SymbolKind::Text && s.address() != 0)
        .map(|s| (base + s.address(), s.name().unwrap_or("?").to_string()))
        .collect();
    // Every defined dynamic symbol is a possible entry (IFUNC symbols, whose
    // resolvers the dynamic linker calls, aren't typed as functions);
    // discovery keeps only those in code.
    roots.extend(file.dynamic_symbols().filter(|s| !s.is_undefined() && s.address() != 0).map(|s| base + s.address()));
    // Landing pads (catch blocks, cleanups), which only the unwinder reaches.
    struct FileBytes<'a>(&'a [u8], &'a [(u64, u64, u64)]);
    impl crate::ehframe::Bytes for FileBytes<'_> {
        fn at(&self, addr: u64, len: usize) -> Option<&[u8]> {
            let &(va, off, _) = self.1.iter().find(|&&(va, _, sz)| addr >= va && addr + len as u64 <= va + sz)?;
            self.0.get((off + addr - va) as usize..(off + addr - va) as usize + len)
        }
    }
    if let Some(eh) = file.section_by_name(".eh_frame") {
        let pads = crate::ehframe::landing_pads(&FileBytes(bytes, &loads), eh.address(), eh.size());
        roots.extend(pads.into_iter().map(|p| base + p));
    }
    // IRELATIVE relocations name IFUNC resolvers by address.
    roots.extend(relocs.iter().filter(|r| r.r_type == 1032).map(|r| base.wrapping_add(r.addend as u64)));
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
        roots,
    })
}

/// Prelinks parsed objects into one image: builds the image-wide symbol
/// table, applies every relocation to final values, and merges segments.
/// The first object is the executable (its program headers win). With
/// `interp`, the last object is the dynamic linker: the image starts there,
/// and relocations we don't model are left for it to apply at run time.
fn link(objects: Vec<Parsed>, interp: bool, libs: Vec<(String, u64, u64)>, names: Vec<String>) -> Result<Image> {
    let placed: Vec<(String, u64, u64)> = names.into_iter().zip(&objects).map(|(n, o)| (n, o.base, o.top)).collect();
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
    let (app_entry, phdr, phnum) = (exe.entry, exe.phdr, exe.phnum);
    let (entry, interp_base) = match objects.last() {
        Some(ld) if interp => (ld.entry, ld.base),
        _ => (app_entry, 0),
    };
    let mut roots = vec![app_entry];
    let mut segments: Vec<Segment> = Vec::new();
    let mut functions = Vec::new();
    let mut code = Vec::new();

    for mut obj in objects {
        for r in &obj.relocs {
            if r.discovery_only {
                roots.push(obj.base.wrapping_add(r.addend as u64));
                continue;
            }
            let value = match r.r_type {
                RELATIVE => obj.base.wrapping_add(r.addend as u64),
                GLOB_DAT | JUMP_SLOT | ABS64 => match &r.sym {
                    // A local defined symbol was folded into the addend.
                    None => obj.base.wrapping_add(r.addend as u64),
                    Some((name, weak)) => match global.get(name) {
                        Some(addr) => addr.wrapping_add(r.addend as u64),
                        // A weak undefined symbol resolves to 0 (the code guards it).
                        // With a dynamic linker, prelinked values only guide code
                        // discovery (it relinks everything), so a missing symbol is
                        // its error to report, at run time, as on Linux.
                        None if *weak || interp => r.addend as u64,
                        None => bail!("undefined symbol `{name}` (provide the library with --lib)"),
                    },
                },
                // TLS and other relocations: the dynamic linker applies them.
                _ if interp => continue,
                other => bail!("unsupported relocation type {other}"),
            };
            if interp && r.r_type == JUMP_SLOT {
                // glibc binds lazily by default, initializing each slot as
                // `*slot += load base` from the file's value (the PLT header),
                // which isn't idempotent: leave the slot as in the file. The
                // resolved function is still a root.
                roots.push(value);
                continue;
            }
            write_word(&mut obj.segments, r.at, value)?;
        }
        roots.append(&mut obj.roots);
        segments.append(&mut obj.segments);
        functions.append(&mut obj.functions);
        code.append(&mut obj.code);
    }
    code.sort_by_key(|r| r.start);
    functions.sort();
    functions.dedup_by_key(|f| f.0);
    Ok(Image { entry, segments, phdr, phnum, functions, code, app_entry, interp_base, roots, libs, objects: placed })
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

/// Decodes one instruction. Hint-space instructions the decoder doesn't know
/// (newer ones, such as CHKFEAT from the Guarded Control Stack extension)
/// execute as NOPs on a CPU without their feature, which is what translated
/// code runs as, so they decode as NOP.
fn decode(word: u32, addr: u64) -> Option<bad64::Instruction> {
    bad64::decode(word, addr).ok().or_else(|| {
        const HINT_MASK: u32 = 0xffff_f01f;
        const HINT: u32 = 0xd503_201f;
        (word & HINT_MASK == HINT).then(|| bad64::decode(HINT, addr).ok()).flatten()
    })
}

impl Image {
    /// Loads a single statically-linked or position-independent executable.
    pub fn load(bytes: &[u8]) -> Result<Image> {
        let base = elf_base(bytes)?;
        link(vec![parse_object(bytes, base)?], false, Vec::new(), vec!["program".into()])
    }

    /// Loads one object of a dynamically-linked program by itself, for
    /// translating it as its own module (docs/abi.md, "Libraries"): a shared
    /// object at `base`, or the executable at its own base. References to
    /// other objects stay unresolved; the dynamic linker binds them at run
    /// time, and the translated code reaches them through `dispatch`.
    pub fn load_object(bytes: &[u8], name: &str, base: Option<u64>) -> Result<Image> {
        let base = match base {
            Some(b) => b,
            None => elf_base(bytes)?,
        };
        let obj = parse_object(bytes, base)?;
        let mut image = link(vec![obj], true, Vec::new(), vec![name.to_string()])?;
        // Not a dynamic linker run first: the object's own entry.
        image.entry = image.app_entry;
        image.interp_base = 0;
        Ok(image)
    }

    /// Loads a dynamically-linked executable together with the shared
    /// objects it needs, prelinking them into one image. `libs` are
    /// (name, bytes) pairs; symbols are resolved across all of them.
    ///
    /// With `interp` (the program's PT_INTERP, e.g. musl's libc.so), the
    /// dynamic linker is translated too and runs first, as the kernel would
    /// start it: it sets up TLS, runs constructors and jumps to the program.
    /// Its own relocation pass over the prelinked image is idempotent (RELA
    /// relocations store absolute values).
    /// `layout` gives bases from an earlier translation (library name →
    /// base), kept where they still fit, so adding a library doesn't move
    /// the others and their translated code stays cached (see `place`).
    pub fn load_dynamic(
        exe: &[u8],
        libs: &[(String, Vec<u8>)],
        interp: Option<&(String, Vec<u8>)>,
        layout: &HashMap<String, u64>,
    ) -> Result<Image> {
        let mut objects = vec![parse_object(exe, elf_base(exe)?)?];
        let start = objects[0].top.next_multiple_of(LIB_ALIGN).max(PIE_BASE);
        let all: Vec<&(String, Vec<u8>)> = libs.iter().chain(interp).collect();
        let sizes = all
            .iter()
            .map(|(name, bytes)| Ok((name.as_str(), extent(bytes).with_context(|| format!("loading {name}"))?)))
            .collect::<Result<Vec<_>>>()?;
        let bases = place(&sizes, start, layout);
        let mut placed = Vec::new();
        for (k, ((name, bytes), base)) in all.into_iter().zip(bases).enumerate() {
            let obj = parse_object(bytes, base).with_context(|| format!("loading {name}"))?;
            if k < libs.len() {
                placed.push((name.clone(), obj.base, obj.top));
            }
            objects.push(obj);
        }
        let names = std::iter::once("program".to_string()).chain(libs.iter().chain(interp).map(|(n, _)| n.clone())).collect();
        link(objects, interp.is_some(), placed, names)
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
                out.push((addr, word, decode(word, addr)));
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
        let image = Image::load_dynamic(&main, &[("dynlib.so".into(), lib)], None, &HashMap::new()).unwrap();
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

#[cfg(test)]
mod place_tests {
    use super::*;

    #[test]
    fn places_new_objects_after_each_other() {
        let bases = place(&[("a", 0x12345), ("b", 0x10000), ("c", 1)], 0x50_0000, &HashMap::new());
        assert_eq!(bases, [0x50_0000, 0x52_0000, 0x53_0000]);
    }

    #[test]
    fn keeps_earlier_bases_so_added_libraries_move_nothing() {
        let layout = HashMap::from([("a".to_string(), 0x50_0000), ("c".to_string(), 0x53_0000)]);
        // "b" is new and comes between them in load order: it goes above
        // everything instead of shifting "c".
        let bases = place(&[("a", 0x12345), ("b", 0x10000), ("c", 1)], 0x50_0000, &layout);
        assert_eq!(bases, [0x50_0000, 0x54_0000, 0x53_0000]);
    }

    #[test]
    fn moves_an_object_that_no_longer_fits() {
        let layout = HashMap::from([("a".to_string(), 0x50_0000), ("c".to_string(), 0x52_0000)]);
        // "a" grew into "c"'s place: "a" keeps its base (it comes first), "c"
        // moves above.
        let bases = place(&[("a", 0x30000), ("c", 0x100)], 0x50_0000, &layout);
        assert_eq!(bases, [0x50_0000, 0x53_0000]);
        // A base below the start (the executable grew) isn't reused.
        let bases = place(&[("a", 0x100)], 0x60_0000, &layout);
        assert_eq!(bases, [0x60_0000]);
    }
}
