//! arm64 → LLVM IR.
//!
//! Code is lifted a region (REGION_SPAN bytes of guest address space) at a
//! time: each region becomes one LLVM function `void @R_<start>(i64 pc)`,
//! entered at any of its blocks (a `switch` on `pc`), whose blocks branch to
//! each other directly, so LLVM optimizes loops within it. Control leaves a
//! region by guaranteed tail call (`musttail`, which the WebAssembly backend
//! compiles to `return_call`), so the host stack never grows however the
//! guest branches, calls or returns: to another region's function for a
//! direct branch, or to `@dispatch` (in the support code) for an indirect
//! one (BR, BLR, RET), which finds the region by binary search. Every
//! function has the same signature, which `musttail` requires. (One
//! function per block would also exceed WebAssembly's limit of a million
//! functions in a module for large programs.)
//!
//! Guest state lives in globals: `@R` (x0–x30 and SP), `@F` (N Z C V),
//! `@V` (v0–v31) and `@TP` (TPIDR_EL0). Within a block, registers are
//! loaded on first use, kept as SSA values, and stored back when the block
//! ends. Guest memory is the
//! WebAssembly memory itself: guest address A is WebAssembly address A.
//!
//! Instruction semantics are in `insn.rs`.

pub mod insn;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Result, bail};
use bad64::{ArrSpec, Condition, Imm, Instruction, Operand, Reg, Shift};

use crate::discover::{Program, imm_u64, is_control};
use crate::elf::Image;

/// A guest register operand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum R {
    /// x0–x30 (index 0–30), or SP (index 31).
    X(u8),
    /// xzr/wzr: reads as zero, writes are discarded.
    Zr,
    /// v0–v31.
    V(u8),
}

/// Decodes a bad64 register into (register, access width in bits).
pub fn reg(r: Reg) -> Result<(R, u32)> {
    let name = r.name();
    Ok(match name {
        "xzr" => (R::Zr, 64),
        "wzr" => (R::Zr, 32),
        "sp" => (R::X(31), 64),
        "wsp" => (R::X(31), 32),
        "fp" => (R::X(29), 64), // bad64's names for x29/x30
        "lr" => (R::X(30), 64),
        _ => {
            let (kind, rest) = name.split_at(1);
            let n: u8 = rest.parse().map_err(|_| anyhow::anyhow!("register {name}"))?;
            match kind {
                "x" => (R::X(n), 64),
                "w" => (R::X(n), 32),
                "q" | "v" => (R::V(n), 128),
                "d" => (R::V(n), 64),
                "s" => (R::V(n), 32),
                "h" => (R::V(n), 16),
                "b" => (R::V(n), 8),
                _ => bail!("unsupported register {name}"),
            }
        }
    })
}

pub fn imm_i64(imm: &Imm) -> i64 {
    imm_u64(imm) as i64
}

/// An LLVM integer constant of width `bits` holding the low bits of `v`.
pub fn konst(v: u64, bits: u32) -> String {
    if bits >= 64 {
        return (v as i64).to_string();
    }
    let shift = 64 - bits;
    (((v << shift) as i64) >> shift).to_string()
}

pub fn konst128(v: u128) -> String {
    (v as i128).to_string()
}

/// Element width of a vector arrangement (`.16b` → 8, `.2d` → 64, …).
pub fn elem_bits(a: &ArrSpec) -> u32 {
    use ArrSpec::*;
    match a {
        Full(_) => 128,
        TwoDoubles(_) | OneDouble(_) => 64,
        FourSingles(_) | TwoSingles(_) | OneSingle(_) => 32,
        EightHalves(_) | FourHalves(_) | TwoHalves(_) | OneHalf(_) => 16,
        SixteenBytes(_) | EightBytes(_) | FourBytes(_) | OneByte(_) => 8,
    }
}

/// Total width of a vector arrangement in bits (64 for the `.8b`-style
/// half-register forms).
pub fn arr_bits(a: &ArrSpec) -> u32 {
    use ArrSpec::*;
    match a {
        TwoSingles(_) | FourHalves(_) | EightBytes(_) => 64,
        OneDouble(_) | OneSingle(_) | OneHalf(_) | OneByte(_) => elem_bits(a),
        _ => 128,
    }
}

pub const N: usize = 0;
pub const Z: usize = 1;
pub const C: usize = 2;
pub const V: usize = 3;

/// The guest address range lifted into one function (a "region"): its blocks
/// branch to each other directly, so LLVM optimizes loops within it.
pub const REGION_SPAN: u64 = 0x1000;

pub fn region_of(addr: u64) -> u64 {
    addr / REGION_SPAN * REGION_SPAN
}

/// Builds the body of one block, a basic block of its region's function.
pub struct Block<'a> {
    pub body: String,
    addr: u64,
    next_tmp: usize,
    /// Every block start, to tell direct jumps to blocks from others.
    leaders: &'a std::collections::BTreeSet<u64>,
    /// Other regions this block tail-calls.
    pub calls: std::collections::BTreeSet<u64>,
    gpr: [Option<String>; 32],
    gpr_dirty: [bool; 32],
    flags: [Option<String>; 4],
    flags_dirty: [bool; 4],
    vec: [Option<String>; 32],
    vec_dirty: [bool; 32],
    /// Private string constants this block needs (for fatal messages),
    /// emitted in the same module as the block: (name, LLVM constant text).
    pub strings: Vec<(String, String)>,
}

impl<'a> Block<'a> {
    /// A block at `addr`, numbering its temporaries from `first_tmp` (unique
    /// within the region's function).
    fn new(addr: u64, first_tmp: usize, leaders: &'a std::collections::BTreeSet<u64>) -> Self {
        Block {
            body: String::new(),
            addr,
            next_tmp: first_tmp,
            leaders,
            calls: Default::default(),
            gpr: Default::default(),
            gpr_dirty: [false; 32],
            flags: Default::default(),
            flags_dirty: [false; 4],
            vec: Default::default(),
            vec_dirty: [false; 32],
            strings: Vec::new(),
        }
    }

    pub fn line(&mut self, s: impl AsRef<str>) {
        self.body.push_str("  ");
        self.body.push_str(s.as_ref());
        self.body.push('\n');
    }

    /// Emits `%tN = <expr>` and returns `%tN`.
    pub fn val(&mut self, expr: impl AsRef<str>) -> String {
        let t = format!("%t{}", self.next_tmp);
        self.next_tmp += 1;
        self.line(format!("{t} = {}", expr.as_ref()));
        t
    }

    pub fn label(&mut self) -> String {
        let l = format!("L{}", self.next_tmp);
        self.next_tmp += 1;
        l
    }

    pub fn place_label(&mut self, l: &str) {
        let _ = writeln!(self.body, "{l}:");
    }

    // --- Guest registers ---------------------------------------------------

    fn get_x(&mut self, i: u8) -> String {
        let i = i as usize;
        if let Some(v) = &self.gpr[i] {
            return v.clone();
        }
        let p = self.val(format!("getelementptr [32 x i64], ptr @R, i64 0, i64 {i}"));
        let v = self.val(format!("load i64, ptr {p}"));
        self.gpr[i] = Some(v.clone());
        v
    }

    fn set_x(&mut self, i: u8, v: String) {
        self.gpr[i as usize] = Some(v);
        self.gpr_dirty[i as usize] = true;
    }

    pub fn get_flag(&mut self, f: usize) -> String {
        if let Some(v) = &self.flags[f] {
            return v.clone();
        }
        let p = self.val(format!("getelementptr [4 x i8], ptr @F, i64 0, i64 {f}"));
        let b = self.val(format!("load i8, ptr {p}"));
        let v = self.val(format!("trunc i8 {b} to i1"));
        self.flags[f] = Some(v.clone());
        v
    }

    pub fn set_flags(&mut self, n: String, z: String, c: String, v: String) {
        for (i, x) in [n, z, c, v].into_iter().enumerate() {
            self.flags[i] = Some(x);
            self.flags_dirty[i] = true;
        }
    }

    fn get_v(&mut self, i: u8) -> String {
        let i = i as usize;
        if let Some(v) = &self.vec[i] {
            return v.clone();
        }
        let p = self.val(format!("getelementptr [32 x i128], ptr @V, i64 0, i64 {i}"));
        let v = self.val(format!("load i128, ptr {p}, align 16"));
        self.vec[i] = Some(v.clone());
        v
    }

    fn set_v(&mut self, i: u8, v: String) {
        self.vec[i as usize] = Some(v);
        self.vec_dirty[i as usize] = true;
    }

    /// Writes all modified registers and flags back to the globals.
    pub fn flush(&mut self) {
        for i in 0..32 {
            if self.gpr_dirty[i] {
                let v = self.gpr[i].clone().unwrap();
                let p = self.val(format!("getelementptr [32 x i64], ptr @R, i64 0, i64 {i}"));
                self.line(format!("store i64 {v}, ptr {p}"));
                self.gpr_dirty[i] = false;
            }
            if self.vec_dirty[i] {
                let v = self.vec[i].clone().unwrap();
                let p = self.val(format!("getelementptr [32 x i128], ptr @V, i64 0, i64 {i}"));
                self.line(format!("store i128 {v}, ptr {p}, align 16"));
                self.vec_dirty[i] = false;
            }
        }
        for f in 0..4 {
            if self.flags_dirty[f] {
                let v = self.flags[f].clone().unwrap();
                let b = self.val(format!("zext i1 {v} to i8"));
                let p = self.val(format!("getelementptr [4 x i8], ptr @F, i64 0, i64 {f}"));
                self.line(format!("store i8 {b}, ptr {p}"));
                self.flags_dirty[f] = false;
            }
        }
    }

    /// Forgets cached values, after a call that may have changed the globals.
    pub fn invalidate(&mut self) {
        self.gpr = Default::default();
        self.flags = Default::default();
        self.vec = Default::default();
    }

    /// Reads a general register as an integer of width `bits`.
    pub fn read(&mut self, r: R, bits: u32) -> Result<String> {
        match r {
            R::Zr => Ok("0".into()),
            R::X(i) => {
                let v = self.get_x(i);
                Ok(if bits == 64 { v } else { self.val(format!("trunc i64 {v} to i{bits}")) })
            }
            R::V(_) => bail!("expected a general register"),
        }
    }

    /// Writes a general register; 32-bit writes zero the upper half.
    pub fn write(&mut self, r: R, v: String, bits: u32) -> Result<()> {
        match r {
            R::Zr => Ok(()),
            R::X(i) => {
                let v = if bits == 64 { v } else { self.val(format!("zext i{bits} {v} to i64")) };
                self.set_x(i, v);
                Ok(())
            }
            R::V(_) => bail!("expected a general register"),
        }
    }

    /// Reads a SIMD/FP register's low `bits` as an integer.
    pub fn read_v(&mut self, i: u8, bits: u32) -> String {
        let v = self.get_v(i);
        if bits == 128 { v } else { self.val(format!("trunc i128 {v} to i{bits}")) }
    }

    /// Writes a SIMD/FP register; narrower writes zero the rest, as on arm64.
    pub fn write_v(&mut self, i: u8, v: String, bits: u32) {
        let v = if bits == 128 { v } else { self.val(format!("zext i{bits} {v} to i128")) };
        self.set_v(i, v);
    }

    /// Reads any register as an integer of its own width.
    pub fn read_any(&mut self, (r, bits): (R, u32)) -> Result<String> {
        match r {
            R::V(i) => Ok(self.read_v(i, bits)),
            _ => self.read(r, bits),
        }
    }

    #[allow(dead_code)]
    pub fn write_any(&mut self, (r, bits): (R, u32), v: String) -> Result<()> {
        match r {
            R::V(i) => {
                self.write_v(i, v, bits);
                Ok(())
            }
            _ => self.write(r, v, bits),
        }
    }

    // --- Operands ------------------------------------------------------------

    pub fn reg_op(&self, i: &Instruction, k: usize) -> Result<(R, u32)> {
        match i.operands().get(k) {
            Some(Operand::Reg { reg: r, .. }) => reg(*r),
            other => bail!("operand {k}: expected a register, got {other:?}"),
        }
    }

    pub fn imm_op(&self, i: &Instruction, k: usize) -> Result<u64> {
        match i.operands().get(k) {
            Some(Operand::Imm32 { imm, shift } | Operand::Imm64 { imm, shift }) => {
                let v = imm_u64(imm);
                Ok(match shift {
                    Some(Shift::LSL(s)) => v << s,
                    // "Shifting ones" (movi/mvni): the vacated bits are set.
                    Some(Shift::MSL(s)) => (v << s) | ((1u64 << s) - 1),
                    None => v,
                    Some(s) => bail!("immediate with shift {s:?}"),
                })
            }
            Some(Operand::Label(imm)) => Ok(imm_u64(imm)),
            other => bail!("operand {k}: expected an immediate, got {other:?}"),
        }
    }

    pub fn cond_op(&mut self, i: &Instruction, k: usize) -> Result<String> {
        match i.operands().get(k) {
            Some(Operand::Cond(c)) => Ok(self.cond(*c)),
            other => bail!("operand {k}: expected a condition, got {other:?}"),
        }
    }

    /// Evaluates a source operand as an integer of width `bits`: a general
    /// register (possibly shifted or extended) or an immediate.
    pub fn operand(&mut self, op: &Operand, bits: u32) -> Result<String> {
        match op {
            Operand::Reg { reg: r, .. } => {
                let (r, rbits) = reg(*r)?;
                let v = self.read(r, rbits)?;
                Ok(self.resize(v, rbits, bits, false))
            }
            Operand::ShiftReg { reg: r, shift } => {
                let (r, rbits) = reg(*r)?;
                let v = self.read(r, rbits)?;
                self.apply_shift(v, rbits, bits, shift)
            }
            Operand::Imm32 { imm, shift } | Operand::Imm64 { imm, shift } => {
                let v = match shift {
                    Some(Shift::LSL(s)) => imm_u64(imm) << s,
                    None => imm_u64(imm),
                    Some(s) => bail!("immediate with shift {s:?}"),
                };
                Ok(konst(v, bits))
            }
            Operand::Label(imm) => Ok(konst(imm_u64(imm), bits)),
            other => bail!("unsupported operand {other:?}"),
        }
    }

    pub fn resize(&mut self, v: String, from: u32, to: u32, signed: bool) -> String {
        use std::cmp::Ordering::*;
        match from.cmp(&to) {
            Equal => v,
            Less => self.val(format!("{} i{from} {v} to i{to}", if signed { "sext" } else { "zext" })),
            Greater => self.val(format!("trunc i{from} {v} to i{to}")),
        }
    }

    /// Shifted-register and extended-register operand forms.
    pub fn apply_shift(&mut self, v: String, rbits: u32, bits: u32, shift: &Shift) -> Result<String> {
        let ext = |b: &mut Self, from: u32, signed: bool, amount: u32| -> String {
            let narrow = b.resize(v.clone(), rbits, from, false);
            let wide = b.resize(narrow, from, bits, signed);
            if amount == 0 { wide } else { b.val(format!("shl i{bits} {wide}, {amount}")) }
        };
        Ok(match *shift {
            Shift::LSL(n) | Shift::LSR(n) | Shift::ASR(n) | Shift::ROR(n) => {
                let v = self.resize(v.clone(), rbits, bits, false);
                if n == 0 {
                    return Ok(v);
                }
                match *shift {
                    Shift::LSL(_) => self.val(format!("shl i{bits} {v}, {n}")),
                    Shift::LSR(_) => self.val(format!("lshr i{bits} {v}, {n}")),
                    Shift::ASR(_) => self.val(format!("ashr i{bits} {v}, {n}")),
                    _ => self.val(format!("call i{bits} @llvm.fshr.i{bits}(i{bits} {v}, i{bits} {v}, i{bits} {n})")),
                }
            }
            Shift::UXTB(n) => ext(self, 8, false, n),
            Shift::UXTH(n) => ext(self, 16, false, n),
            Shift::UXTW(n) => ext(self, 32.min(rbits), false, n),
            Shift::UXTX(n) => ext(self, rbits, false, n),
            Shift::SXTB(n) => ext(self, 8, true, n),
            Shift::SXTH(n) => ext(self, 16, true, n),
            Shift::SXTW(n) => ext(self, 32.min(rbits), true, n),
            Shift::SXTX(n) => ext(self, rbits, true, n),
            _ => bail!("unsupported shift {shift:?}"),
        })
    }

    // --- Flags and conditions ---------------------------------------------

    /// arm64 AddWithCarry: returns a + b + carry, setting N Z C V if asked.
    pub fn add_with_carry(&mut self, a: &str, b: &str, carry: &str, bits: u32, set: bool) -> String {
        let w = bits * 2;
        let ua = self.val(format!("zext i{bits} {a} to i{w}"));
        let ub = self.val(format!("zext i{bits} {b} to i{w}"));
        let uc = self.val(format!("zext i1 {carry} to i{w}"));
        let us0 = self.val(format!("add i{w} {ua}, {ub}"));
        let us = self.val(format!("add i{w} {us0}, {uc}"));
        let r = self.val(format!("trunc i{w} {us} to i{bits}"));
        if set {
            let sa = self.val(format!("sext i{bits} {a} to i{w}"));
            let sb = self.val(format!("sext i{bits} {b} to i{w}"));
            let ss0 = self.val(format!("add i{w} {sa}, {sb}"));
            let ss = self.val(format!("add i{w} {ss0}, {uc}"));
            let n = self.val(format!("icmp slt i{bits} {r}, 0"));
            let z = self.val(format!("icmp eq i{bits} {r}, 0"));
            let ur = self.val(format!("zext i{bits} {r} to i{w}"));
            let c = self.val(format!("icmp ne i{w} {us}, {ur}"));
            let sr = self.val(format!("sext i{bits} {r} to i{w}"));
            let v = self.val(format!("icmp ne i{w} {ss}, {sr}"));
            self.set_flags(n, z, c, v);
        }
        r
    }

    pub fn logic_flags(&mut self, r: &str, bits: u32) {
        let n = self.val(format!("icmp slt i{bits} {r}, 0"));
        let z = self.val(format!("icmp eq i{bits} {r}, 0"));
        self.set_flags(n, z, "false".into(), "false".into());
    }

    pub fn not(&mut self, x: String) -> String {
        self.val(format!("xor i1 {x}, true"))
    }

    pub fn cond(&mut self, c: Condition) -> String {
        use Condition::*;
        let code = c as u32;
        let is = |k: Condition| code == k as u32;
        if is(EQ) || is(NE) {
            let z = self.get_flag(Z);
            return if is(EQ) { z } else { self.not(z) };
        }
        if is(CS) || is(CC) {
            let c = self.get_flag(C);
            return if is(CS) { c } else { self.not(c) };
        }
        if is(MI) || is(PL) {
            let n = self.get_flag(N);
            return if is(MI) { n } else { self.not(n) };
        }
        if is(VS) || is(VC) {
            let v = self.get_flag(V);
            return if is(VS) { v } else { self.not(v) };
        }
        if is(HI) || is(LS) {
            let c = self.get_flag(C);
            let z = self.get_flag(Z);
            let nz = self.not(z);
            let hi = self.val(format!("and i1 {c}, {nz}"));
            return if is(HI) { hi } else { self.not(hi) };
        }
        if is(GE) || is(LT) {
            let n = self.get_flag(N);
            let v = self.get_flag(V);
            let ge = self.val(format!("icmp eq i1 {n}, {v}"));
            return if is(GE) { ge } else { self.not(ge) };
        }
        if is(GT) || is(LE) {
            let n = self.get_flag(N);
            let v = self.get_flag(V);
            let z = self.get_flag(Z);
            let ge = self.val(format!("icmp eq i1 {n}, {v}"));
            let nz = self.not(z);
            let gt = self.val(format!("and i1 {ge}, {nz}"));
            return if is(GT) { gt } else { self.not(gt) };
        }
        "true".into()
    }

    // --- Memory ------------------------------------------------------------------

    /// Computes the address of a memory operand, applying pre/post-index
    /// writeback to the base register.
    pub fn address(&mut self, op: &Operand) -> Result<String> {
        let base = |b: &mut Self, r: &Reg| -> Result<String> {
            let (r, _) = reg(*r)?;
            b.read(r, 64)
        };
        Ok(match op {
            Operand::MemReg(r) => base(self, r)?,
            Operand::MemOffset { reg: r, offset, .. } => {
                let b = base(self, r)?;
                match imm_i64(offset) {
                    0 => b,
                    off => self.val(format!("add i64 {b}, {off}")),
                }
            }
            Operand::MemPreIdx { reg: r, imm } => {
                let b = base(self, r)?;
                let a = self.val(format!("add i64 {b}, {}", imm_i64(imm)));
                self.write(reg(*r)?.0, a.clone(), 64)?;
                a
            }
            Operand::MemPostIdxImm { reg: r, imm } => {
                let b = base(self, r)?;
                let next = self.val(format!("add i64 {b}, {}", imm_i64(imm)));
                self.write(reg(*r)?.0, next, 64)?;
                b
            }
            // Post-index by a register: [xn], xm.
            Operand::MemPostIdxReg([r, step]) => {
                let b = base(self, r)?;
                let s = base(self, step)?;
                let next = self.val(format!("add i64 {b}, {s}"));
                self.write(reg(*r)?.0, next, 64)?;
                b
            }
            Operand::MemExt { regs, shift, .. } => {
                let b = base(self, &regs[0])?;
                let (ir, ibits) = reg(regs[1])?;
                let idx = self.read(ir, ibits)?;
                let off = match shift {
                    None => self.resize(idx, ibits, 64, false),
                    Some(s) => self.apply_shift(idx, ibits, 64, s)?,
                };
                self.val(format!("add i64 {b}, {off}"))
            }
            Operand::Label(imm) => konst(imm_u64(imm), 64),
            other => bail!("unsupported memory operand {other:?}"),
        })
    }

    pub fn load(&mut self, addr: &str, bits: u32) -> String {
        let p = self.val(format!("inttoptr i64 {addr} to ptr"));
        self.val(format!("load i{bits}, ptr {p}, align 1"))
    }

    pub fn store(&mut self, addr: &str, v: &str, bits: u32) {
        let p = self.val(format!("inttoptr i64 {addr} to ptr"));
        self.line(format!("store i{bits} {v}, ptr {p}, align 1"));
    }

    // --- Control flow ------------------------------------------------------------

    /// Ends the block by jumping to a known address: a branch within the
    /// region, a tail call to another region's function, or, for an address
    /// that isn't a block, `@dispatch` (which reports it).
    pub fn jump(&mut self, target: u64) {
        self.flush();
        if self.leaders.contains(&target) {
            let region = region_of(target);
            if region == region_of(self.addr) {
                self.line(format!("br label %b_{target:x}"));
                return;
            }
            self.calls.insert(region);
            self.line(format!("musttail call void @R_{region:x}(i64 {})", target as i64));
        } else {
            self.line(format!("musttail call void @dispatch(i64 {})", target as i64));
        }
        self.line("ret void");
    }

    /// Ends the block by jumping to a computed address.
    pub fn jump_to(&mut self, target: String) {
        self.flush();
        self.line(format!("musttail call void @dispatch(i64 {target})"));
        self.line("ret void");
    }

    pub fn branch(&mut self, cond: String, taken: u64, not_taken: u64) {
        self.flush();
        let (t, f) = (self.label(), self.label());
        self.line(format!("br i1 {cond}, label %{t}, label %{f}"));
        self.place_label(&t);
        self.jump(taken);
        self.place_label(&f);
        self.jump(not_taken);
    }

    /// Stops the program at an instruction that can't be translated. The
    /// message is a private constant in the block's own module.
    pub fn fail(&mut self, pc: u64, why: &str) {
        let name = format!("@S_{:x}_{}", self.addr, self.strings.len());
        self.strings.push((name.clone(), cstring(why.as_bytes())));
        self.flush();
        self.line(format!("call void @__wasm_env_fatal(ptr {name}, i64 {})", pc as i64));
        self.line("unreachable");
    }

    /// A call into the support code that may change guest registers.
    pub fn call_support(&mut self, callee: &str) {
        self.flush();
        self.line(format!("call void @{callee}()"));
        self.invalidate();
    }
}

/// The guest address range whose blocks go in one compilation module. Bounds
/// clang's memory (a whole translated library can be hundreds of thousands
/// of blocks, infeasible as one module). Grouping by address rather than by
/// count keeps chunk boundaries stable: translating again with a few more
/// blocks changes only the chunks containing them, so the object cache
/// (link.rs) reuses the rest.
const CHUNK_SPAN: u64 = 0x8000;

/// A lifted region: the text of its function, the private string constants
/// (fatal messages) its blocks use, and the other regions it tail-calls.
struct LiftedRegion {
    start: u64,
    text: String,
    strings: Vec<(String, String)>,
    calls: std::collections::BTreeSet<u64>,
}

pub struct Lifted {
    /// The data module: guest state, the guest image, block-function
    /// declarations, and the dispatch table (block address -> function).
    pub data: String,
    /// Code modules, each defining a subset of the block functions.
    pub chunks: Vec<String>,
    /// Instructions that couldn't be translated, by mnemonic.
    pub unsupported: BTreeMap<String, (usize, String)>,
    pub blocks: usize,
}

impl Lifted {
    /// All modules joined, for `emit`/debugging (not separately compilable).
    pub fn ir(&self) -> String {
        let mut s = self.data.clone();
        for c in &self.chunks {
            s.push_str("\n; ---- chunk ----\n");
            s.push_str(c);
        }
        s
    }
}

/// Lifts every block. Control flow goes through `@dispatch` (defined in the
/// support code as a binary search over the block table), so block
/// functions are independent and split across modules for compilation.
pub fn lift(image: &Image, prog: &Program) -> Lifted {
    let mut unsupported = BTreeMap::new();
    let mut regions: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for &leader in &prog.leaders {
        regions.entry(region_of(leader)).or_default().push(leader);
    }

    let mut lifted = Vec::new();
    for (&region, leaders) in &regions {
        // The region's function: entered at any of its blocks (the dispatcher
        // and other regions pass the block's address), then branching within.
        let mut text = format!("define void @R_{region:x}(i64 %pc) {{\nentry:\n  switch i64 %pc, label %bad [\n");
        for &a in leaders {
            let _ = writeln!(text, "    i64 {}, label %b_{a:x}", a as i64);
        }
        text.push_str("  ]\n");
        let (mut tmp, mut strings, mut calls) = (0, Vec::new(), std::collections::BTreeSet::new());
        for &start in leaders {
            let mut b = Block::new(start, tmp, &prog.leaders);
            let mut pc = start;
            loop {
                let Some(i) = prog.insns.get(&pc) else {
                    b.fail(pc, "undecodable instruction");
                    break;
                };
                match insn::lift(&mut b, i) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(e) => {
                        unsupported.entry(i.op().mnem().to_string()).or_insert_with(|| (0, format!("`{i}`: {e}"))).0 +=
                            1;
                        b.fail(pc, &format!("unsupported instruction `{i}`: {e}"));
                        break;
                    }
                }
                pc += 4;
                if prog.leaders.contains(&pc) || is_control(i) {
                    b.jump(pc);
                    break;
                }
            }
            tmp = b.next_tmp;
            let _ = write!(text, "b_{start:x}:\n{}", b.body);
            strings.append(&mut b.strings);
            calls.append(&mut b.calls);
        }
        text.push_str("bad:\n  call void @__wasm_env_bad_target(i64 %pc)\n  unreachable\n}\n\n");
        lifted.push(LiftedRegion { start: region, text, strings, calls });
    }

    let chunks = build_chunks(&lifted);
    let data = build_data(image, prog);
    Lifted { data, chunks, unsupported, blocks: prog.leaders.len() }
}

/// Assembles the code modules: region functions grouped into chunks, each
/// with the declarations it needs.
fn build_chunks(regions: &[LiftedRegion]) -> Vec<String> {
    let preamble = format!(
        "target triple = \"wasm64-unknown-unknown\"\n\
         @R = external global [32 x i64]\n\
         @F = external global [4 x i8]\n\
         @V = external global [32 x i128]\n\
         @TP = external global i64\n\
         declare void @dispatch(i64)\n\
         declare void @__wasm_env_svc()\n\
         declare void @__wasm_env_fatal(ptr, i64)\n\
         declare void @__wasm_env_bad_target(i64)\n\
         declare i32 @__wasm_env_crc32(i32, i64, i32, i32)\n{}",
        intrinsics()
    );
    let mut groups: BTreeMap<u64, Vec<&LiftedRegion>> = BTreeMap::new();
    for r in regions {
        groups.entry(r.start / CHUNK_SPAN).or_default().push(r);
    }
    groups
        .into_values()
        .map(|group| {
            let mut m = preamble.clone();
            // Regions this chunk calls but doesn't define.
            let defined: std::collections::BTreeSet<u64> = group.iter().map(|r| r.start).collect();
            let called: std::collections::BTreeSet<u64> = group.iter().flat_map(|r| r.calls.iter().copied()).collect();
            for c in called.difference(&defined) {
                let _ = writeln!(m, "declare void @R_{c:x}(i64)");
            }
            for r in group {
                for (name, text) in &r.strings {
                    let _ = writeln!(m, "{name} = private constant {text}");
                }
                m.push_str(&r.text);
            }
            m
        })
        .collect()
}

/// Builds the data module: guest state, the guest image, and the dispatch
/// table mapping each block address to its function.
fn build_data(image: &Image, prog: &Program) -> String {
    let mut ir = String::from("target triple = \"wasm64-unknown-unknown\"\n");
    ir.push_str("@R = global [32 x i64] zeroinitializer, align 8\n");
    ir.push_str("@F = global [4 x i8] zeroinitializer\n");
    ir.push_str("@V = global [32 x i128] zeroinitializer, align 16\n");
    ir.push_str("@TP = global i64 0\n\n");

    // The segments' bytes aren't here: they're added to the linked module as
    // data segments at their own addresses (link::add_data_segments), so the
    // data pointers are null.
    ir.push_str("%seg = type { i64, i64, i64, ptr }\n");
    let _ = write!(ir, "@guest_segments = constant [{} x %seg] [", image.segments.len());
    for (k, s) in image.segments.iter().enumerate() {
        let _ = write!(
            ir,
            "{}%seg {{ i64 {}, i64 {}, i64 {}, ptr null }}",
            if k == 0 { "" } else { ", " },
            s.vaddr as i64,
            s.data.len(),
            s.memsz as i64
        );
    }
    let _ = writeln!(ir, "]\n@guest_segment_count = constant i64 {}", image.segments.len());
    let _ = writeln!(ir, "@guest_entry = constant i64 {}", image.entry as i64);
    let _ = writeln!(ir, "@guest_phdr = constant i64 {}", image.phdr.unwrap_or(0) as i64);
    let _ = writeln!(ir, "@guest_app_entry = constant i64 {}", image.app_entry as i64);
    let _ = writeln!(ir, "@guest_interp_base = constant i64 {}", image.interp_base as i64);

    // Libraries the dynamic linker will map: the support code places each
    // at its prelinked base (see __wasm_env_svc in support/runtime.c).
    ir.push_str("%lib = type { ptr, i64, i64 }\n");
    for (k, (path, _, _)) in image.libs.iter().enumerate() {
        let _ = writeln!(ir, "@libpath{k} = private constant {}", cstring(path.as_bytes()));
    }
    let entries: Vec<String> = image
        .libs
        .iter()
        .enumerate()
        .map(|(k, (_, base, end))| format!("%lib {{ ptr @libpath{k}, i64 {}, i64 {} }}", *base as i64, *end as i64))
        .collect();
    let table = if entries.is_empty() { "zeroinitializer".to_string() } else { format!("[{}]", entries.join(", ")) };
    let _ = writeln!(ir, "@guest_libs = constant [{} x %lib] {table}", image.libs.len());
    let _ = writeln!(ir, "@guest_lib_count = constant i64 {}", image.libs.len());

    // Every object, for error messages.
    for (k, (name, _, _)) in image.objects.iter().enumerate() {
        let _ = writeln!(ir, "@objname{k} = private constant {}", cstring(name.as_bytes()));
    }
    let objects: Vec<String> = image
        .objects
        .iter()
        .enumerate()
        .map(|(k, (_, base, end))| format!("%lib {{ ptr @objname{k}, i64 {}, i64 {} }}", *base as i64, *end as i64))
        .collect();
    let table = if objects.is_empty() { "zeroinitializer".to_string() } else { format!("[{}]", objects.join(", ")) };
    let _ = writeln!(ir, "@guest_objects = constant [{} x %lib] {table}", image.objects.len());
    let _ = writeln!(ir, "@guest_object_count = constant i64 {}", image.objects.len());
    let _ = writeln!(ir, "@guest_phnum = constant i64 {}\n", image.phnum);

    // Declare every region function, then build the dispatch table: parallel
    // arrays of sorted region addresses and their functions, searched by the
    // support code's `dispatch`. A region function checks that the address
    // it's entered at is one of its blocks.
    let regions: std::collections::BTreeSet<u64> = prog.leaders.iter().map(|&a| region_of(a)).collect();
    for r in &regions {
        let _ = writeln!(ir, "declare void @R_{r:x}(i64)");
    }
    let n = regions.len();
    let _ = write!(ir, "@dispatch_addrs = constant [{n} x i64] [");
    for (i, &r) in regions.iter().enumerate() {
        let _ = write!(ir, "{}i64 {}", if i == 0 { "" } else { ", " }, r as i64);
    }
    ir.push_str("]\n");
    let _ = write!(ir, "@dispatch_fns = constant [{n} x ptr] [");
    for (i, &r) in regions.iter().enumerate() {
        let _ = write!(ir, "{}ptr @R_{r:x}", if i == 0 { "" } else { ", " });
    }
    ir.push_str("]\n");
    let _ = writeln!(ir, "@dispatch_count = constant i64 {n}");
    ir
}

/// The LLVM intrinsic declarations the lifted code may call.
fn intrinsics() -> String {
    let mut ir = String::new();
    for t in ["i32", "i64"] {
        let _ = writeln!(
            ir,
            "declare {t} @llvm.fshr.{t}({t}, {t}, {t})\n\
             declare {t} @llvm.ctlz.{t}({t}, i1)\n\
             declare {t} @llvm.bswap.{t}({t})\n\
             declare {t} @llvm.fptosi.sat.{t}.f64(double)\n\
             declare {t} @llvm.fptoui.sat.{t}.f64(double)\n\
             declare {t} @llvm.fptosi.sat.{t}.f32(float)\n\
             declare {t} @llvm.fptoui.sat.{t}.f32(float)\n\
             declare {t} @llvm.fptosi.sat.{t}.f16(half)\n\
             declare {t} @llvm.fptoui.sat.{t}.f16(half)"
        );
    }
    ir.push_str("declare i16 @llvm.bswap.i16(i16)\n");
    for t in ["i8", "i16", "i32", "i64"] {
        let _ = writeln!(
            ir,
            "declare {t} @llvm.umin.{t}({t}, {t})\n\
             declare {t} @llvm.umax.{t}({t}, {t})\n\
             declare {t} @llvm.smin.{t}({t}, {t})\n\
             declare {t} @llvm.smax.{t}({t}, {t})\n\
             declare {t} @llvm.ctpop.{t}({t})\n\
             declare {t} @llvm.uadd.sat.{t}({t}, {t})\n\
             declare {t} @llvm.sadd.sat.{t}({t}, {t})\n\
             declare {t} @llvm.usub.sat.{t}({t}, {t})\n\
             declare {t} @llvm.ssub.sat.{t}({t}, {t})\n\
             declare {t} @llvm.bitreverse.{t}({t})"
        );
    }
    for (t, s) in [("double", "f64"), ("float", "f32"), ("half", "f16")] {
        let _ = writeln!(
            ir,
            "declare {t} @llvm.sqrt.{s}({t})\n\
             declare {t} @llvm.fabs.{s}({t})\n\
             declare {t} @llvm.fma.{s}({t}, {t}, {t})\n\
             declare {t} @llvm.minnum.{s}({t}, {t})\n\
             declare {t} @llvm.maxnum.{s}({t}, {t})\n\
             declare {t} @llvm.rint.{s}({t})\n\
             declare {t} @llvm.roundeven.{s}({t})\n\
             declare {t} @llvm.trunc.{s}({t})\n\
             declare {t} @llvm.ceil.{s}({t})\n\
             declare {t} @llvm.floor.{s}({t})\n\
             declare {t} @llvm.copysign.{s}({t}, {t})"
        );
    }
    ir
}

/// An LLVM array constant `[N x i8] c"..."` (no added NUL).
fn cstring_raw(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2 + 4);
    s.push_str("c\"");
    for &b in bytes {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            s.push(b as char);
        } else {
            let _ = write!(s, "\\{b:02X}");
        }
    }
    s.push('"');
    s
}

/// An LLVM `[N x i8] c"..."` constant with a trailing NUL (for C strings).
fn cstring(bytes: &[u8]) -> String {
    format!("[{} x i8] {}", bytes.len() + 1, cstring_raw(&[bytes, &[0]].concat()))
}
