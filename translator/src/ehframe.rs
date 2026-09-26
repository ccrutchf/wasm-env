//! Landing pads from exception tables: code that only the unwinder jumps
//! to (C++ `catch` blocks and cleanups that run destructors), which no
//! branch or stored pointer reveals to discovery.
//!
//! `.eh_frame` holds a CIE per compilation unit and an FDE per function;
//! an FDE whose CIE has an 'L' augmentation points at the function's LSDA
//! (in `.gcc_except_table`), whose call-site table lists each landing pad as
//! an offset from its landing-pad base (the function's start, unless the
//! LSDA says otherwise). The unwinder resumes at `base + landing pad` by an
//! indirect branch, so each is a block root.

/// Reads guest bytes at a link-time address (within the object's file).
pub trait Bytes {
    fn at(&self, addr: u64, len: usize) -> Option<&[u8]>;
}

struct Reader<'a, B: Bytes> {
    mem: &'a B,
    addr: u64,
}

// DW_EH_PE_* pointer encodings.
const OMIT: u8 = 0xff;
const PCREL: u8 = 0x10;
const DATAREL: u8 = 0x30;
const INDIRECT: u8 = 0x80;

impl<B: Bytes> Reader<'_, B> {
    fn u8(&mut self) -> Option<u8> {
        let b = self.mem.at(self.addr, 1)?[0];
        self.addr += 1;
        Some(b)
    }
    fn fixed(&mut self, n: usize) -> Option<u64> {
        let b = self.mem.at(self.addr, n)?;
        self.addr += n as u64;
        Some(b.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64))
    }
    fn uleb(&mut self) -> Option<u64> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = self.u8()?;
            if shift < 64 {
                v |= u64::from(b & 0x7f) << shift;
            }
            shift += 7;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
    }
    fn sleb(&mut self) -> Option<i64> {
        let (mut v, mut shift) = (0i64, 0);
        loop {
            let b = self.u8()?;
            if shift < 64 {
                v |= i64::from(b & 0x7f) << shift;
            }
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && b & 0x40 != 0 {
                    v |= -1 << shift;
                }
                return Some(v);
            }
        }
    }
    /// A value in pointer encoding `enc`, as an address (for pc-relative
    /// ones, relative to where it's stored).
    fn pointer(&mut self, enc: u8) -> Option<u64> {
        if enc == OMIT {
            return Some(0);
        }
        let here = self.addr;
        let v = match enc & 0x0f {
            0x0 => self.fixed(8)?,
            0x1 => self.uleb()?,
            0x2 => self.fixed(2)?,
            0x3 => self.fixed(4)?,
            0x4 => self.fixed(8)?,
            0x9 => self.sleb()? as u64,
            0xa => self.fixed(2)? as i16 as u64,
            0xb => self.fixed(4)? as i32 as u64,
            0xc => self.fixed(8)?,
            _ => return None,
        };
        let v = match enc & 0x70 {
            0 => v,
            PCREL => here.wrapping_add(v),
            DATAREL => return None, // not used for code on arm64
            _ => return None,
        };
        if enc & INDIRECT != 0 {
            let b = self.mem.at(v, 8)?;
            return Some(u64::from_le_bytes(b.try_into().ok()?));
        }
        Some(v)
    }
}

/// The landing pads of every function `.eh_frame` (at `start`, `len`
/// bytes) describes, as link-time addresses.
pub fn landing_pads<B: Bytes>(mem: &B, start: u64, len: u64) -> Vec<u64> {
    let mut pads = Vec::new();
    // LSDA encoding by CIE address; None if the CIE has no LSDA.
    let mut cies: std::collections::HashMap<u64, (u8, Option<u8>)> = std::collections::HashMap::new();
    let mut at = start;
    while at + 4 <= start + len {
        let mut r = Reader { mem, addr: at };
        let Some(mut length) = r.fixed(4) else { break };
        if length == 0 {
            break; // terminator
        }
        if length == 0xffff_ffff {
            let Some(l) = r.fixed(8) else { break };
            length = l;
        }
        let body = r.addr;
        let next = body + length;
        let id_at = r.addr;
        let Some(id) = r.fixed(4) else { break };
        if id == 0 {
            if let Some(cie) = parse_cie(&mut r) {
                cies.insert(at, cie);
            }
        } else {
            // An FDE: the CIE is `id` bytes back from the id field.
            let cie_at = id_at.wrapping_sub(id);
            if let Some(&(fde_enc, Some(lsda_enc))) = cies.get(&cie_at)
                && let Some(pc_begin) = r.pointer(fde_enc)
                && r.pointer(fde_enc & 0x0f).is_some() // pc_range
                && r.uleb().is_some() // augmentation data length ('z')
                && let Some(lsda) = r.pointer(lsda_enc)
                && lsda != 0
            {
                lsda_pads(mem, lsda, pc_begin, &mut pads);
            }
        }
        at = next;
    }
    pads.sort();
    pads.dedup();
    pads
}

/// A CIE's FDE pointer encoding and LSDA encoding (if it has an 'L').
fn parse_cie<B: Bytes>(r: &mut Reader<B>) -> Option<(u8, Option<u8>)> {
    let version = r.u8()?;
    let mut aug = Vec::new();
    loop {
        let c = r.u8()?;
        if c == 0 {
            break;
        }
        aug.push(c);
    }
    if aug.first() != Some(&b'z') {
        return None; // no augmentation data: no LSDA
    }
    r.uleb()?; // code alignment
    r.sleb()?; // data alignment
    if version == 1 {
        r.u8()?;
    } else {
        r.uleb()?;
    } // return address register
    r.uleb()?; // augmentation data length
    let (mut fde_enc, mut lsda_enc) = (0u8, None);
    for &c in &aug[1..] {
        match c {
            b'L' => lsda_enc = Some(r.u8()?),
            b'R' => fde_enc = r.u8()?,
            b'P' => {
                let enc = r.u8()?;
                r.pointer(enc & !INDIRECT)?;
            }
            b'S' | b'B' | b'G' => {}
            _ => return None,
        }
    }
    Some((fde_enc, lsda_enc))
}

/// Adds the landing pads of the LSDA at `lsda` (for the function at
/// `func`) to `pads`.
fn lsda_pads<B: Bytes>(mem: &B, lsda: u64, func: u64, pads: &mut Vec<u64>) -> Option<()> {
    let mut r = Reader { mem, addr: lsda };
    let lp_enc = r.u8()?;
    let lp_start = if lp_enc == OMIT { func } else { r.pointer(lp_enc)? };
    let ttype_enc = r.u8()?;
    if ttype_enc != OMIT {
        r.uleb()?; // offset to the type table
    }
    let cs_enc = r.u8()?;
    let cs_len = r.uleb()?;
    let end = r.addr + cs_len;
    while r.addr < end {
        r.pointer(cs_enc & 0x0f)?; // start
        r.pointer(cs_enc & 0x0f)?; // length
        let pad = r.pointer(cs_enc & 0x0f)?;
        r.uleb()?; // action
        if pad != 0 {
            pads.push(lp_start.wrapping_add(pad));
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(u64, Vec<u8>);
    impl Bytes for Mem {
        fn at(&self, addr: u64, len: usize) -> Option<&[u8]> {
            let off = addr.checked_sub(self.0)? as usize;
            self.1.get(off..off + len)
        }
    }

    #[test]
    fn finds_landing_pads_through_fde_and_lsda() {
        // At 0x1000: a CIE "zLR" (LSDA and FDE pointers pc-relative sdata4),
        // an FDE for a function at 0x2000 whose LSDA is at 0x1100, then a
        // terminator. The LSDA has two call sites, one with a landing pad at
        // +0x40, one without.
        let mut m = vec![0u8; 0x200];
        let cie: Vec<u8> = [
            &[0u8; 4][..],             // length, patched below
            &0u32.to_le_bytes(),       // CIE id
            &[1],                      // version
            b"zLR\0",                  // augmentation
            &[4, 0x78, 30],            // code align 4, data align -8, return reg 30
            &[2, 0x1b, 0x1b],          // augmentation data: LSDA and FDE encodings
        ]
        .concat();
        let mut cie = cie;
        let cie_len = (cie.len() - 4) as u32;
        cie[..4].copy_from_slice(&cie_len.to_le_bytes());
        m[..cie.len()].copy_from_slice(&cie);
        let fde_at = cie.len();
        let id = (fde_at + 4) as u32; // back to the CIE at 0
        let pc_field = 0x1000 + fde_at as u64 + 8;
        let lsda_field = pc_field + 4 + 4 + 1;
        let fde: Vec<u8> = [
            &17u32.to_le_bytes()[..],
            &id.to_le_bytes(),
            &((0x2000 - pc_field as i64) as i32).to_le_bytes(), // pc_begin
            &0x100u32.to_le_bytes(),                           // pc_range
            &[4],                                              // augmentation length
            &((0x1100 - lsda_field as i64) as i32).to_le_bytes(),
        ]
        .concat();
        m[fde_at..fde_at + fde.len()].copy_from_slice(&fde);
        // The LSDA: lpstart omitted, no type table, call sites uleb128.
        let lsda = [0xff, 0xff, 0x01, 8, 0x00, 0x10, 0x40, 0x01, 0x10, 0x08, 0x00, 0x00];
        m[0x100..0x100 + lsda.len()].copy_from_slice(&lsda);
        let mem = Mem(0x1000, m);
        assert_eq!(landing_pads(&mem, 0x1000, (fde_at + fde.len() + 4) as u64), [0x2040]);
    }
}
