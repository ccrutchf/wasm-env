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
//! Guest state lives in a block per guest thread (see STATE_POINTERS): `%R`
//! (x0–x30 and SP), `%F` (N Z C V), `%V` (v0–v31) and `%TP` (TPIDR_EL0).
//! Within a block, registers are
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

/// An FP result awaiting its exception check (see Block::check_fp).
struct FpCheck {
    ty: String,
    /// 0 for a scalar, else the number of lanes of vector values.
    lanes: u32,
    inputs: Vec<String>,
    r: String,
    div: bool,
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
    /// FPSR exception flags raised in this block and not yet stored (i32).
    fpsr_raised: Option<String>,
    /// FP results to check for exceptions at the next flush (see check_fp),
    /// and whether any of them was NaN or infinite (i1).
    fp_pending: Vec<FpCheck>,
    fp_special: Option<String>,
    /// Flag liveness facts (see `lift`): whether the block reads NZCV before
    /// setting them, whether it sets them, the addresses it jumps to
    /// directly, and whether it jumps anywhere else.
    pub flags_read_first: bool,
    pub flags_set: bool,
    pub succs: Vec<u64>,
    pub indirect: bool,
    /// Set while flushing before a system call, whose flag stores are kept.
    keep_flags: bool,
    /// The region is translated alone (a page patched at run time, see
    /// `lift`), so other regions' functions aren't in its module: jumps
    /// to them go through `dispatch`.
    alone: bool,
    /// Private string constants this block needs (for fatal messages),
    /// emitted in the same module as the block: (name, LLVM constant text).
    pub strings: Vec<(String, String)>,
}

impl<'a> Block<'a> {
    /// A block at `addr`, numbering its temporaries from `first_tmp` (unique
    /// within the region's function).
    fn new(addr: u64, first_tmp: usize, leaders: &'a std::collections::BTreeSet<u64>, alone: bool) -> Self {
        Block {
            body: String::new(),
            addr,
            alone,
            next_tmp: first_tmp,
            leaders,
            calls: Default::default(),
            gpr: Default::default(),
            gpr_dirty: [false; 32],
            flags: Default::default(),
            flags_dirty: [false; 4],
            vec: Default::default(),
            vec_dirty: [false; 32],
            fpsr_raised: None,
            fp_pending: Vec::new(),
            fp_special: None,
            flags_read_first: false,
            flags_set: false,
            succs: Vec::new(),
            indirect: false,
            keep_flags: false,
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
        let p = self.val(format!("getelementptr inbounds nuw [32 x i64], ptr %R, i64 0, i64 {i}"));
        let v = self.val(format!("load i64, ptr {p}{REG}"));
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
        if !self.flags_set {
            self.flags_read_first = true;
        }
        let p = self.val(format!("getelementptr inbounds nuw [4 x i8], ptr %F, i64 0, i64 {f}"));
        let b = self.val(format!("load i8, ptr {p}{REG}"));
        let v = self.val(format!("trunc i8 {b} to i1"));
        self.flags[f] = Some(v.clone());
        v
    }

    pub fn set_flags(&mut self, n: String, z: String, c: String, v: String) {
        self.flags_set = true;
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
        let p = self.val(format!("getelementptr inbounds nuw [32 x i128], ptr %V, i64 0, i64 {i}"));
        let v = self.val(format!("load i128, ptr {p}, align 16{REG}"));
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
                let p = self.val(format!("getelementptr inbounds nuw [32 x i64], ptr %R, i64 0, i64 {i}"));
                self.line(format!("store i64 {v}, ptr {p}{REG}"));
                self.gpr_dirty[i] = false;
            }
            if self.vec_dirty[i] {
                let v = self.vec[i].clone().unwrap();
                let p = self.val(format!("getelementptr inbounds nuw [32 x i128], ptr %V, i64 0, i64 {i}"));
                self.line(format!("store i128 {v}, ptr {p}, align 16{REG}"));
                self.vec_dirty[i] = false;
            }
        }
        // Flag stores are marked: `lift` drops them where no later block can
        // read them (see flag liveness there). Those before a system call stay.
        let marked = !self.keep_flags && self.flags_dirty.iter().any(|&d| d);
        if marked {
            self.line(";FLAGS");
        }
        for f in 0..4 {
            if self.flags_dirty[f] {
                let v = self.flags[f].clone().unwrap();
                let b = self.val(format!("zext i1 {v} to i8"));
                let p = self.val(format!("getelementptr inbounds nuw [4 x i8], ptr %F, i64 0, i64 {f}"));
                self.line(format!("store i8 {b}, ptr {p}{REG}"));
                self.flags_dirty[f] = false;
            }
        }
        if marked {
            self.line(";ENDFLAGS");
        }
        self.resolve_fp_checks();
        if let Some(raised) = self.fpsr_raised.take() {
            let old = self.val("load i32, ptr %FPSR");
            let new = self.val(format!("or i32 {old}, {raised}"));
            self.line(format!("store i32 {new}, ptr %FPSR"));
        }
    }

    /// Records an FP result whose exceptions (invalid, divide by zero,
    /// overflow: see __wasm_env_fp_flags in support/runtime.c) FPSR must
    /// reflect. The common case, a finite result, costs a comparison here;
    /// the flags are worked out a few instructions later, only if some
    /// recorded result was NaN or infinite.
    pub fn check_fp(&mut self, ty: &str, inputs: &[&str], r: &str, div: bool) {
        let (s, inf) = match ty {
            "double" => ("f64", "0x7FF0000000000000"),
            "float" => ("f32", "0x7FF0000000000000"),
            _ => ("f16", "0xH7C00"),
        };
        let a = self.val(format!("call {ty} @llvm.fabs.{s}({ty} {r})"));
        let special = self.fcmp("ueq", ty, &a, inf);
        let special = match self.fp_special.take() {
            Some(old) => self.val(format!("or i1 {old}, {special}")),
            None => special,
        };
        self.fp_special = Some(special);
        self.fp_pending.push(FpCheck {
            ty: ty.to_string(),
            lanes: 0,
            inputs: inputs.iter().map(|x| x.to_string()).collect(),
            r: r.to_string(),
            div,
        });
    }

    /// `fcmp pred ty x, y`, comparing half-precision values as float (which
    /// is exact): the WebAssembly backend all but hangs on comparisons of
    /// half vectors (LLVM 21), which LLVM's vectorizer forms from lane-wise
    /// code. The fence keeps LLVM from narrowing the comparison back to half.
    pub fn fcmp(&mut self, pred: &str, ty: &str, x: &str, y: &str) -> String {
        if ty == "half" {
            let [x, y] = [x, y].map(|v| {
                let f = self.val(format!("fpext half {v} to float"));
                self.val(format!("call float @llvm.arithmetic.fence.f32(float {f})"))
            });
            return self.val(format!("fcmp {pred} float {x}, {y}"));
        }
        self.val(format!("fcmp {pred} {ty} {x}, {y}"))
    }

    /// How many FP results await their exception check.
    pub fn pending_fp_checks(&self) -> usize {
        self.fp_pending.len()
    }

    /// check_fp for vectors of `lanes` elements of type `ty`: one vector
    /// comparison, reduced to a bit.
    pub fn check_fp_vec(&mut self, ty: &str, lanes: u32, inputs: &[&str], r: &str, div: bool) {
        let (s, inf) = if ty == "double" { ("f64", "0x7FF0000000000000") } else { ("f32", "0x7FF0000000000000") };
        let vt = format!("<{lanes} x {ty}>");
        let a = self.val(format!("call {vt} @llvm.fabs.v{lanes}{s}({vt} {r})"));
        let lanes_special = self.val(format!("fcmp ueq {vt} {a}, splat ({ty} {inf})"));
        let special = self.val(format!("call i1 @llvm.vector.reduce.or.v{lanes}i1(<{lanes} x i1> {lanes_special})"));
        let special = match self.fp_special.take() {
            Some(old) => self.val(format!("or i1 {old}, {special}")),
            None => special,
        };
        self.fp_special = Some(special);
        self.fp_pending.push(FpCheck {
            ty: ty.to_string(),
            lanes,
            inputs: inputs.iter().map(|x| x.to_string()).collect(),
            r: r.to_string(),
            div,
        });
    }

    /// Works out the flags of the recorded FP results (see check_fp), if any
    /// was NaN or infinite, with one branch. The lifter calls it every few
    /// instructions; flushing does too.
    pub fn resolve_fp_checks(&mut self) {
        let Some(special) = self.fp_special.take() else { return };
        let pending = std::mem::take(&mut self.fp_pending);
        let (pre, slow, done) = (self.label(), self.label(), self.label());
        self.line(format!("br label %{pre}"));
        self.place_label(&pre);
        self.line(format!("br i1 {special}, label %{slow}, label %{done}"));
        self.place_label(&slow);
        let mut bits = "0".to_string();
        for c in &pending {
            // Each lane (or the scalar), its values widened to double.
            for k in 0..c.lanes.max(1) {
                let arg = |b: &mut Self, v: &str| {
                    let v = if c.lanes == 0 {
                        v.to_string()
                    } else {
                        b.val(format!("extractelement <{} x {}> {v}, i32 {k}", c.lanes, c.ty))
                    };
                    if c.ty == "double" { v } else { b.val(format!("fpext {} {v} to double", c.ty)) }
                };
                let mut args: Vec<String> = c.inputs.iter().map(|x| arg(self, x)).collect();
                args.resize(3, "0.0".into());
                let r = arg(self, &c.r);
                let kind = c.inputs.len() as u32 | (c.div as u32) << 2;
                let f = self.val(format!(
                    "call i32 @__wasm_env_fp_flags(double {}, double {}, double {}, double {r}, i32 {kind})",
                    args[0], args[1], args[2]
                ));
                bits = self.val(format!("or i32 {bits}, {f}"));
            }
        }
        self.line(format!("br label %{done}"));
        self.place_label(&done);
        let raised = self.val(format!("phi i32 [ 0, %{pre} ], [ {bits}, %{slow} ]"));
        self.raise_fp(raised);
    }

    /// Raises FPSR cumulative exception flags (an i32 of FPSR bits).
    pub fn raise_fp(&mut self, bits: String) {
        let v = match self.fpsr_raised.take() {
            Some(old) => self.val(format!("or i32 {old}, {bits}")),
            None => bits,
        };
        self.fpsr_raised = Some(v);
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
    /// Computed at the operands' width (wasm has no i128, which widening a
    /// 64-bit sum would need): the carry out is `r < a` (`r <= a` with a
    /// carry in), and signed overflow is when both operands' signs differ
    /// from the result's.
    pub fn add_with_carry(&mut self, a: &str, b: &str, carry: &str, bits: u32, set: bool) -> String {
        let c_in = self.val(format!("zext i1 {carry} to i{bits}"));
        let s = self.val(format!("add i{bits} {a}, {b}"));
        let r = self.val(format!("add i{bits} {s}, {c_in}"));
        if set {
            let n = self.val(format!("icmp slt i{bits} {r}, 0"));
            let z = self.val(format!("icmp eq i{bits} {r}, 0"));
            let below = self.val(format!("icmp ult i{bits} {r}, {a}"));
            let at_or_below = self.val(format!("icmp ule i{bits} {r}, {a}"));
            let c = self.val(format!("select i1 {carry}, i1 {at_or_below}, i1 {below}"));
            let ar = self.val(format!("xor i{bits} {a}, {r}"));
            let br = self.val(format!("xor i{bits} {b}, {r}"));
            let both = self.val(format!("and i{bits} {ar}, {br}"));
            let v = self.val(format!("icmp slt i{bits} {both}, 0"));
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
                // `nuw` lets the offset become the WebAssembly access's
                // offset immediate, so accesses off one base register share
                // its masked index. (An access that wraps past 2^64 is
                // outside the masked address space anyway.)
                match imm_i64(offset) {
                    0 => b,
                    off if off > 0 => self.val(format!("add nuw i64 {b}, {off}")),
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
        self.val(format!("load i{bits}, ptr {p}, align 1{MEM}"))
    }

    pub fn store(&mut self, addr: &str, v: &str, bits: u32) {
        let p = self.val(format!("inttoptr i64 {addr} to ptr"));
        self.line(format!("store i{bits} {v}, ptr {p}, align 1{MEM}"));
    }

    // Atomic accesses (acquire/release loads and stores, exclusives, LSE)
    // become WebAssembly atomics, which are sequentially consistent: at
    // least as strong as any arm64 ordering. Like arm64's, they must be
    // naturally aligned (a misaligned one traps).

    pub fn load_atomic(&mut self, addr: &str, bits: u32) -> String {
        let p = self.val(format!("inttoptr i64 {addr} to ptr"));
        self.val(format!("load atomic i{bits}, ptr {p} seq_cst, align {}{MEM}", bits / 8))
    }

    pub fn store_atomic(&mut self, addr: &str, v: &str, bits: u32) {
        let p = self.val(format!("inttoptr i64 {addr} to ptr"));
        self.line(format!("store atomic i{bits} {v}, ptr {p} seq_cst, align {}{MEM}", bits / 8));
    }

    /// `atomicrmw op`: returns the old value.
    pub fn rmw(&mut self, op: &str, addr: &str, v: &str, bits: u32) -> String {
        let p = self.val(format!("inttoptr i64 {addr} to ptr"));
        self.val(format!("atomicrmw {op} ptr {p}, i{bits} {v} seq_cst, align {}{MEM}", bits / 8))
    }

    /// Compare-and-swap: returns (old value, whether it stored).
    pub fn cmpxchg(&mut self, addr: &str, expected: &str, new: &str, bits: u32) -> (String, String) {
        let p = self.val(format!("inttoptr i64 {addr} to ptr"));
        let pair =
            self.val(format!("cmpxchg ptr {p}, i{bits} {expected}, i{bits} {new} seq_cst seq_cst, align {}{MEM}", bits / 8));
        let old = self.val(format!("extractvalue {{ i{bits}, i1 }} {pair}, 0"));
        let ok = self.val(format!("extractvalue {{ i{bits}, i1 }} {pair}, 1"));
        (old, ok)
    }

    // --- Control flow ------------------------------------------------------------

    /// Ends the block by jumping to a known address: a branch within the
    /// region, a tail call to another region's function, or, for an address
    /// that isn't a block, `@dispatch` (which reports it).
    pub fn jump(&mut self, target: u64) {
        self.succs.push(target);
        self.flush();
        if self.leaders.contains(&target) && (!self.alone || region_of(target) == region_of(self.addr)) {
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

    /// Ends the block by jumping to a computed address: through the dispatch
    /// cache (see `dispatch` in support/runtime.c, whose hash this matches),
    /// probed here so a hit costs no call into the dispatcher, else through
    /// `dispatch`.
    pub fn jump_to(&mut self, target: String) {
        self.indirect = true;
        self.flush();
        let region = self.val(format!("lshr i64 {target}, 12"));
        let high = self.val(format!("lshr i64 {region}, 12"));
        let mixed = self.val(format!("xor i64 {region}, {high}"));
        let slot = self.val(format!("and i64 {mixed}, 4095"));
        let tag_at = self.val(format!("getelementptr inbounds nuw [4096 x {{ i64, ptr }}], ptr %DC, i64 0, i64 {slot}, i32 0"));
        let tag = self.val(format!("load i64, ptr {tag_at}"));
        let want = self.val(format!("add i64 {region}, 1"));
        let hit = self.val(format!("icmp eq i64 {tag}, {want}"));
        let (cached, miss) = (self.label(), self.label());
        self.line(format!("br i1 {hit}, label %{cached}, label %{miss}"));
        self.place_label(&cached);
        let fn_at = self.val(format!("getelementptr inbounds nuw [4096 x {{ i64, ptr }}], ptr %DC, i64 0, i64 {slot}, i32 1"));
        let f = self.val(format!("load ptr, ptr {fn_at}"));
        self.line(format!("musttail call void {f}(i64 {target})"));
        self.line("ret void");
        self.place_label(&miss);
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
    /// A system call (SVC) whose next instruction is at `next`. The support
    /// code may redirect control (a vfork child that exec'd or exited
    /// resumes its parent after the parent's clone), returning where to go
    /// on, else 0.
    pub fn svc(&mut self, next: u64) {
        self.keep_flags = true;
        self.flush();
        self.keep_flags = false;
        let to = self.val(format!("call i64 @__wasm_env_svc_at(i64 {})", next as i64));
        self.invalidate();
        let redirect = self.val(format!("icmp ne i64 {to}, 0"));
        let (away, on) = (self.label(), self.label());
        self.line(format!("br i1 {redirect}, label %{away}, label %{on}"));
        self.place_label(&away);
        self.line(format!("musttail call void @dispatch(i64 {to})"));
        self.line("ret void");
        self.place_label(&on);
    }
}

/// What flag liveness needs to know about a block (see Block's fields).
struct FlagFacts {
    read_first: bool,
    set: bool,
    succs: Vec<u64>,
    unknown: bool,
}

/// The blocks whose NZCV flags some later block may read, so they must be
/// stored when the block ends. Flags are live into a block that reads them
/// before setting them, or doesn't set them and passes them on; live out of
/// one with an unknown successor, or a successor they're live into (a
/// target that isn't a block counts as reading them).
fn flags_live_out(facts: &std::collections::HashMap<u64, FlagFacts>) -> std::collections::HashSet<u64> {
    use std::collections::{HashMap, HashSet};
    let mut preds: HashMap<u64, Vec<u64>> = HashMap::new();
    for (&b, f) in facts {
        for &s in &f.succs {
            preds.entry(s).or_default().push(b);
        }
    }
    let mut live_in: HashSet<u64> = facts.iter().filter(|(_, f)| f.read_first).map(|(&b, _)| b).collect();
    let live_out_of = |b: &u64, live_in: &HashSet<u64>| {
        let f = &facts[b];
        f.unknown || f.succs.iter().any(|s| !facts.contains_key(s) || live_in.contains(s))
    };
    // Worklist: when a block becomes live-in, its predecessors that pass
    // flags through may too.
    let mut work: Vec<u64> = live_in.iter().copied().collect();
    work.extend(facts.iter().filter(|(_, f)| f.unknown || f.succs.iter().any(|s| !facts.contains_key(s))).map(|(&b, _)| b));
    while let Some(b) = work.pop() {
        let f = &facts[&b];
        if !live_in.contains(&b) && !f.set && live_out_of(&b, &live_in) {
            live_in.insert(b);
        } else if !live_in.contains(&b) {
            continue;
        }
        for p in preds.get(&b).into_iter().flatten() {
            if !live_in.contains(p) && !facts[p].set {
                work.push(*p);
            }
        }
    }
    facts.keys().filter(|b| live_out_of(b, &live_in)).copied().collect()
}

/// The guest address range whose blocks go in one compilation module. Bounds
/// clang's memory (a whole translated library can be hundreds of thousands
/// of blocks, infeasible as one module). Grouping by address rather than by
/// count keeps chunk boundaries stable: translating again with a few more
/// blocks changes only the chunks containing them, so the object cache
/// (link.rs) reuses the rest.
const CHUNK_SPAN: u64 = 0x8000;

pub struct Lifted {
    /// The data module: guest state, the guest image, block-function
    /// declarations, and the dispatch table (block address -> function).
    pub data: String,
    /// How many code modules (chunks) went to the sink.
    pub chunks: usize,
    /// Instructions that couldn't be translated, by mnemonic.
    pub unsupported: BTreeMap<String, (usize, String)>,
    pub blocks: usize,
}

/// A block lifted: its IR body, flag liveness facts, and whether it stores
/// to guest memory.
struct LiftedBlock {
    start: u64,
    body: String,
    facts: FlagFacts,
    stores: bool,
}

/// A region's blocks, lifted, with what its function needs besides.
struct RegionIr {
    blocks: Vec<LiftedBlock>,
    strings: Vec<(String, String)>,
    calls: std::collections::BTreeSet<u64>,
    /// Blocks other regions branch to directly (entries of their regions).
    cross: Vec<u64>,
    /// Backward branches within the region: (target, block, end of block).
    loop_edges: Vec<(u64, u64, u64)>,
}

/// Lifts a region's blocks (the same text every time: the lifting is
/// deterministic). Instructions it can't lift are counted in `unsupported`.
fn lift_region(prog: &Program, region: u64, leaders: &[u64], alone: bool, unsupported: &mut BTreeMap<String, (usize, String)>) -> RegionIr {
    let mut out = RegionIr {
        blocks: Vec::new(),
        strings: Vec::new(),
        calls: Default::default(),
        cross: Vec::new(),
        loop_edges: Vec::new(),
    };
    let mut tmp = 0;
    for &start in leaders {
        let mut b = Block::new(start, tmp, &prog.leaders, alone);
        let mut pc = start;
        let mut last = "";
        loop {
            let Some(i) = prog.insns.get(&pc) else {
                b.fail(pc, "undecodable instruction");
                break;
            };
            last = i.op().mnem();
            match insn::lift(&mut b, &i) {
                Ok(true) => break,
                // FP exception checks resolve every few instructions (one
                // branch for all of them), and at the block's end: often
                // enough that their values needn't stay live across a large
                // block (clang's time grows fast with that), rarely enough
                // not to split an unrolled kernel into tiny blocks.
                Ok(false) => {
                    if b.pending_fp_checks() >= 16 {
                        b.resolve_fp_checks();
                    }
                }
                Err(e) => {
                    unsupported.entry(i.op().mnem().to_string()).or_insert_with(|| (0, format!("`{i}`: {e}"))).0 += 1;
                    b.fail(pc, &format!("unsupported instruction `{i}`: {e}"));
                    break;
                }
            }
            pc += 4;
            if prog.leaders.contains(&pc) || is_control(&i) {
                b.jump(pc);
                break;
            }
        }
        tmp = b.next_tmp;
        out.cross.extend(b.succs.iter().copied().filter(|&s| region_of(s) != region));
        out.loop_edges.extend(b.succs.iter().filter(|&&s| region_of(s) == region && s <= start).map(|&s| (s, start, pc)));
        // Calls and returns don't carry flags (the arm64 procedure call
        // standard leaves NZCV undefined across them, and compiled code
        // never reads them on entry), so they don't propagate liveness;
        // other indirect jumps (jump tables) might, so they're assumed to.
        let (succs, unknown) = match last {
            "bl" | "blr" | "blraa" | "blrab" | "ret" | "retaa" | "retab" => (Vec::new(), false),
            _ => (std::mem::take(&mut b.succs), b.indirect),
        };
        let body = std::mem::take(&mut b.body);
        let stores = body.lines().any(|l| {
            let l = l.trim_start();
            (l.starts_with("store ") && l.ends_with(MEM)) || l.contains("atomicrmw ") || l.contains("cmpxchg ")
        });
        out.blocks.push(LiftedBlock {
            start,
            body,
            facts: FlagFacts { read_first: b.flags_read_first, set: b.flags_set, succs, unknown },
            stores,
        });
        out.strings.append(&mut b.strings);
        out.calls.append(&mut b.calls);
    }
    out
}

/// Lifts the program. The code modules ("chunks") go to `sink` one at a
/// time, so a huge library (libtorch has tens of millions of instructions)
/// never has all its IR in memory: a first pass lifts every block for its
/// flag liveness facts and region entries, keeping no IR; a second lifts
/// each chunk's regions again and assembles them.
pub fn lift(image: &Image, prog: &Program, library: bool, page: Option<u64>, sink: &mut dyn FnMut(String)) -> Lifted {
    let mut unsupported = BTreeMap::new();
    let mut regions: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for &leader in &prog.leaders {
        regions.entry(region_of(leader)).or_default().push(leader);
    }

    // Pass 1. Blocks entered other than by a branch or fall-through within
    // their region: those discovery found, plus targets of other regions'
    // branches.
    let mut facts: std::collections::HashMap<u64, FlagFacts> = std::collections::HashMap::new();
    let mut entries = prog.entries.clone();
    // Small loops that load guest memory but never store to it are how
    // compiled code polls a word another thread writes (a `volatile` flag,
    // a relaxed atomic): their loads are made volatile, or LLVM, which knows
    // guest memory and registers don't alias (TBAA), would read the word
    // once, before the loop. Larger store-free loops (a reduction, a search)
    // keep ordinary loads.
    const POLL_LOOP_BYTES: u64 = 64;
    let mut polling = std::collections::HashSet::new();
    // (For one page, only its own region: its blocks are all taken as
    // entries and as leaving their flags live, which is always correct,
    // rather than lifting the whole program to find out.)
    for (&region, leaders) in regions.iter().filter(|(r, _)| page.is_none_or(|p| region_of(p) == **r)) {
        let r = lift_region(prog, region, leaders, false, &mut unsupported);
        entries.extend(r.cross);
        if page.is_some() {
            entries.extend(leaders);
        }
        for &(target, _, end) in &r.loop_edges {
            if end + 4 - target > POLL_LOOP_BYTES {
                continue;
            }
            let in_loop: Vec<&LiftedBlock> = r.blocks.iter().filter(|b| (target..=end).contains(&b.start)).collect();
            if !in_loop.iter().any(|b| b.stores) {
                polling.extend(in_loop.iter().map(|b| b.start));
            }
        }
        for b in r.blocks {
            facts.insert(b.start, b.facts);
        }
    }
    let live_out = match page {
        Some(_) => facts.keys().copied().collect(),
        None => flags_live_out(&facts),
    };
    drop(facts);

    // Pass 2: each region's function, entered at its entry blocks (the
    // dispatcher and other regions pass the block's address), then
    // branching within. Only real entries are cases of the switch, so LLVM
    // can keep registers in SSA across the region's other blocks. (A jump to
    // any other block reaches `bad`, which reports it as a missed target,
    // learned by the next translation.) Blocks whose flags no successor
    // reads drop their flag stores.
    let preamble = chunk_preamble();
    let mut groups: BTreeMap<u64, Vec<(u64, &Vec<u64>)>> = BTreeMap::new();
    for (&region, leaders) in &regions {
        // With `page`, only that page's region: a patch for a program whose
        // translation missed a block there, made at run time.
        if page.is_some_and(|p| region_of(p) != region) {
            continue;
        }
        groups.entry(region / CHUNK_SPAN).or_default().push((region, leaders));
    }
    let mut chunks = 0;
    for group in groups.into_values() {
        let (mut text, mut strings, mut calls) = (String::new(), Vec::new(), std::collections::BTreeSet::new());
        let mut defined = std::collections::BTreeSet::new();
        for (region, leaders) in group {
            let mut r = lift_region(prog, region, leaders, page.is_some(), &mut BTreeMap::new());
            defined.insert(region);
            let _ = write!(text, "define void @R_{region:x}(i64 %pc) {{\nentry:\n{STATE_POINTERS}  switch i64 %pc, label %bad [\n");
            for &a in leaders.iter().filter(|a| entries.contains(a)) {
                let _ = writeln!(text, "    i64 {}, label %b_{a:x}", a as i64);
            }
            text.push_str("  ]\n");
            for b in &r.blocks {
                let _ = writeln!(text, "b_{:x}:", b.start);
                let keep = live_out.contains(&b.start);
                let polls = polling.contains(&b.start);
                let mut in_flags = false;
                for line in b.body.lines() {
                    match line.trim() {
                        ";FLAGS" => in_flags = true,
                        ";ENDFLAGS" => in_flags = false,
                        _ if in_flags && !keep => {}
                        l if polls && l.ends_with(MEM) && l.contains(" = load ") && !l.contains(" = load atomic ") => {
                            text.push_str(&line.replacen(" = load ", " = load volatile ", 1));
                            text.push('\n');
                        }
                        _ => {
                            text.push_str(line);
                            text.push('\n');
                        }
                    }
                }
            }
            text.push_str("bad:\n  call void @__wasm_env_bad_target(i64 %pc)\n  unreachable\n}\n\n");
            strings.append(&mut r.strings);
            calls.append(&mut r.calls);
        }
        let mut m = preamble.clone();
        // Regions this chunk calls but doesn't define.
        for c in calls.difference(&defined) {
            let _ = writeln!(m, "declare void @R_{c:x}(i64)");
        }
        for (name, s) in &strings {
            let _ = writeln!(m, "{name} = private constant {s}");
        }
        m.push_str(&text);
        sink(m);
        chunks += 1;
    }

    let data = match page {
        Some(p) => {
            let mut ir = String::from("target triple = \"wasm64-unknown-unknown\"\n");
            dispatch_table(&mut ir, prog, Some(region_of(p)));
            ir
        }
        None if library => library_data(prog),
        None => build_data(image, prog),
    };
    Lifted { data, chunks, unsupported, blocks: prog.leaders.len() }
}

/// Assembles the code modules: region functions grouped into chunks, each
/// with the declarations it needs.
/// Type-based alias information for every module: guest registers (the
/// `@R`, `@V`, `@F` globals) and guest memory are different types, so LLVM
/// knows a guest load or store never touches a register. Without it every
/// store through a guest pointer might change any register, and registers
/// must be reloaded; with it, LLVM keeps them in wasm locals across a
/// region's blocks and out of its loops. (Sound: guest memory lies below the
/// support code's globals, see link.rs.)
const TBAA: &str = "!900 = !{!\"wasm-env\"}\n\
                    !901 = !{!\"guest register\", !900, i64 0}\n\
                    !902 = !{!901, !901, i64 0}\n\
                    !903 = !{!\"guest memory\", !900, i64 0}\n\
                    !904 = !{!903, !903, i64 0}\n\
                    !906 = !{}\n";
/// Guest state is a block per thread (`struct guest_state` in
/// support/runtime.c, whose layout this matches) at the address in the
/// WebAssembly global `__wasm_env_state`, which the support code sets when a
/// thread starts and which every module of the program shares. Each region
/// function reads it once, on entry (it never changes while code runs), and
/// names the fields.
const STATE_POINTERS: &str = "  %st = load i64, ptr addrspace(1) @__wasm_env_state, align 8, !invariant.load !906\n\
                              \x20 %R = inttoptr i64 %st to ptr\n\
                              \x20 %F = getelementptr inbounds nuw i8, ptr %R, i64 256\n\
                              \x20 %FPSR = getelementptr inbounds nuw i8, ptr %R, i64 260\n\
                              \x20 %TP = getelementptr inbounds nuw i8, ptr %R, i64 264\n\
                              \x20 %MONITOR = getelementptr inbounds nuw i8, ptr %R, i64 272\n\
                              \x20 %V = getelementptr inbounds nuw i8, ptr %R, i64 288\n\
                              \x20 %DC = getelementptr inbounds nuw i8, ptr %R, i64 800\n";

/// The global holding this thread's guest state address (see
/// STATE_POINTERS), defined by the program's data module, and how the
/// support code (C, which can't name a WebAssembly global) sets it.
pub const STATE_GLOBAL: &str = "@__wasm_env_state = addrspace(1) global i64 0\n\
                                define void @__wasm_env_set_state(ptr %p) {\n\
                                \x20 %a = ptrtoint ptr %p to i64\n\
                                \x20 store i64 %a, ptr addrspace(1) @__wasm_env_state\n\
                                \x20 ret void\n\
                                }\n\n";

/// Access tags (see TBAA).
pub const REG: &str = ", !tbaa !902";
pub const MEM: &str = ", !tbaa !904";

/// What every code module starts with: the target, guest state, the
/// support code's entry points, TBAA metadata and intrinsics.
fn chunk_preamble() -> String {
    format!(
        "target triple = \"wasm64-unknown-unknown\"\n\
         @__wasm_env_state = external addrspace(1) global i64\n\
         declare void @dispatch(i64)\n\
         declare void @__wasm_env_svc()\n\
         declare i64 @__wasm_env_svc_at(i64)\n\
         declare i32 @__wasm_env_fp_flags(double, double, double, double, i32) memory(none)\n\
         declare void @__wasm_env_fatal(ptr, i64)\n\
         declare void @__wasm_env_bad_target(i64)\n\
         declare i32 @__wasm_env_crc32(i32, i64, i32, i32) memory(none)\n\
         declare i64 @__wasm_env_counter()\n\
         declare void @__wasm_env_ic_ivau(i64)\n\
         declare void @__wasm_env_isb()\n\
         {}{}",
        TBAA,
        intrinsics()
    )
}

/// Builds the data module: guest state, the guest image, and the dispatch
/// table mapping each block address to its function.
fn build_data(image: &Image, prog: &Program) -> String {
    let mut ir = String::from("target triple = \"wasm64-unknown-unknown\"\n");
    ir.push_str(STATE_GLOBAL);

    // The segments' bytes aren't here: they go in the linked module's
    // guest image section (link::add_image), so the data pointers are null.
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

    dispatch_table(&mut ir, prog, None);
    ir
}

/// The data module of a library translated as its own module (docs/abi.md,
/// "Libraries"): only its dispatch table, which the runtime finds through
/// the module's exports and registers with the program's `dispatch`.
fn library_data(prog: &Program) -> String {
    let mut ir = String::from("target triple = \"wasm64-unknown-unknown\"\n");
    dispatch_table(&mut ir, prog, None);
    ir
}

/// (With `only`, just that region: see `lift`'s `page`.)
fn dispatch_table(ir: &mut String, prog: &Program, only: Option<u64>) {
    // Declare every region function, then build the dispatch table: parallel
    // arrays of sorted region addresses and their functions, searched by the
    // support code's `dispatch`. A region function checks that the address
    // it's entered at is one of its blocks.
    let regions: std::collections::BTreeSet<u64> = prog.leaders.iter().map(|&a| region_of(a)).filter(|&r| only.is_none_or(|o| o == r)).collect();
    for r in &regions {
        let _ = writeln!(*ir, "declare void @R_{r:x}(i64)");
    }
    let n = regions.len();
    let _ = write!(*ir, "@dispatch_addrs = constant [{n} x i64] [");
    for (i, &r) in regions.iter().enumerate() {
        let _ = write!(*ir, "{}i64 {}", if i == 0 { "" } else { ", " }, r as i64);
    }
    ir.push_str("]\n");
    let _ = write!(*ir, "@dispatch_fns = constant [{n} x ptr] [");
    for (i, &r) in regions.iter().enumerate() {
        let _ = write!(*ir, "{}ptr @R_{r:x}", if i == 0 { "" } else { ", " });
    }
    ir.push_str("]\n");
    let _ = writeln!(*ir, "@dispatch_count = constant i64 {n}");
}

/// The LLVM intrinsic declarations the lifted code may call.
fn intrinsics() -> String {
    let mut ir = String::new();
    // Fused multiply-add (see insn::fused).
    ir.push_str("declare <2 x double> @llvm.wasm.relaxed.madd.v2f64(<2 x double>, <2 x double>, <2 x double>)\n");
    ir.push_str("declare <4 x float> @llvm.wasm.relaxed.madd.v4f32(<4 x float>, <4 x float>, <4 x float>)\n");
    ir.push_str("declare float @llvm.arithmetic.fence.f32(float)\n"); // see Block::fcmp
    ir.push_str("declare double @llvm.arithmetic.fence.f64(double)\n");
    // Whole-vector FP (see insn::fp_vec_in).
    for (v, s) in [("<2 x double>", "v2f64"), ("<4 x float>", "v4f32")] {
        for f in ["minimum", "maximum"] {
            let _ = writeln!(ir, "declare {v} @llvm.{f}.{s}({v}, {v})");
        }
        let _ = writeln!(ir, "declare {v} @llvm.fabs.{s}({v})");
    }
    for n in [2, 4] {
        let _ = writeln!(ir, "declare i1 @llvm.vector.reduce.or.v{n}i1(<{n} x i1>)");
    }
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
             declare {t} @llvm.minimum.{s}({t}, {t})\n\
             declare {t} @llvm.maximum.{s}({t}, {t})\n\
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
