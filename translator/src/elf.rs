//! Loading the parts of an arm64 ELF executable the translator needs.

use anyhow::{Context, Result, bail};
use object::elf;
use object::read::elf::{ElfFile64, FileHeader, ProgramHeader};
use object::{Endianness, Object, ObjectSymbol, SymbolKind};

pub struct Segment {
    pub vaddr: u64,
    /// Bytes from the file; the rest of `memsz` is zero (.bss).
    pub data: Vec<u8>,
    pub memsz: u64,
    pub exec: bool,
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
}

impl Image {
    pub fn load(bytes: &[u8]) -> Result<Image> {
        let file = ElfFile64::<Endianness>::parse(bytes).context("not a 64-bit ELF file")?;
        let header = file.elf_header();
        let endian = file.endian();
        if header.e_machine(endian) != elf::EM_AARCH64 {
            bail!("not an arm64 binary (e_machine {})", header.e_machine(endian));
        }
        if header.e_type(endian) != elf::ET_EXEC {
            // Static PIE and dynamically linked binaries come later (M3).
            bail!("only statically linked, non-PIE executables are supported so far");
        }
        let phoff = header.e_phoff(endian);
        let mut segments = Vec::new();
        let mut interp = false;
        let mut phdr = None;
        for ph in file.elf_program_headers() {
            match ph.p_type(endian) {
                elf::PT_INTERP => interp = true,
                elf::PT_LOAD => {
                    let offset = ph.p_offset(endian);
                    let filesz = ph.p_filesz(endian);
                    let vaddr = ph.p_vaddr(endian);
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
                _ => {}
            }
        }
        if interp {
            bail!("dynamically linked binaries aren't supported yet (M3)");
        }
        let mut functions: Vec<(u64, String)> = file
            .symbols()
            .filter(|s| s.kind() == SymbolKind::Text && s.address() != 0)
            .map(|s| (s.address(), s.name().unwrap_or("?").to_string()))
            .collect();
        functions.sort();
        functions.dedup_by_key(|f| f.0);
        Ok(Image { entry: header.e_entry(endian), segments, phdr, phnum: header.e_phnum(endian), functions })
    }

    /// Decodes every 4-byte-aligned word in executable segments. arm64
    /// instructions are fixed-width, so this finds every instruction; words
    /// that don't decode are data (literal pools) or unsupported encodings.
    pub fn decode_all(&self) -> Vec<(u64, u32, Option<bad64::Instruction>)> {
        let mut out = Vec::new();
        for seg in self.segments.iter().filter(|s| s.exec) {
            for (i, word) in seg.data.chunks_exact(4).enumerate() {
                let addr = seg.vaddr + i as u64 * 4;
                let word = u32::from_le_bytes(word.try_into().unwrap());
                out.push((addr, word, bad64::decode(word, addr).ok()));
            }
        }
        out
    }

    pub fn function_at(&self, addr: u64) -> Option<&str> {
        let i = self.functions.partition_point(|f| f.0 <= addr);
        i.checked_sub(1).map(|i| self.functions[i].1.as_str())
    }
}
