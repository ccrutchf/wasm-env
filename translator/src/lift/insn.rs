//! Semantics of individual arm64 instructions, following the Arm
//! Architecture Reference Manual's pseudocode. `lift` returns Ok(true) when
//! the instruction ended the block (control flow); data-processing
//! instructions return Ok(false). An unsupported instruction returns Err,
//! which the caller turns into a trap that names it.

use anyhow::{Result, bail};
use bad64::{Condition, Instruction, Operand, Reg, Shift};

use super::{Block, C, N, R, REG, V, Z, arr_bits, elem_bits, konst, konst128, reg};
use crate::discover::{direct_target, imm_u64};

pub fn lift(b: &mut Block, i: &Instruction) -> Result<bool> {
    let pc = i.address();
    let m = i.op().mnem();
    let ops = i.operands();

    // Conditional branch b.<cond>.
    if let Some(c) = m.strip_prefix("b.") {
        let cond = b.cond(cond_from(c)?);
        b.branch(cond, direct_target(i).unwrap(), pc + 4);
        return Ok(true);
    }

    match m {
        // --- Control flow -----------------------------------------------------
        "b" => {
            b.jump(direct_target(i).unwrap());
            return Ok(true);
        }
        "bl" => {
            b.write(R::X(30), konst(pc + 4, 64), 64)?;
            b.jump(direct_target(i).unwrap());
            return Ok(true);
        }
        "br" | "blr" => {
            let (r, _) = b.reg_op(i, 0)?;
            let target = b.read(r, 64)?;
            if m == "blr" {
                b.write(R::X(30), konst(pc + 4, 64), 64)?;
            }
            b.jump_to(target);
            return Ok(true);
        }
        "ret" => {
            let r = if ops.is_empty() { R::X(30) } else { b.reg_op(i, 0)?.0 };
            let target = b.read(r, 64)?;
            b.jump_to(target);
            return Ok(true);
        }
        "cbz" | "cbnz" => {
            let (r, bits) = b.reg_op(i, 0)?;
            let v = b.read(r, bits)?;
            let cmp = if m == "cbz" { "eq" } else { "ne" };
            let cond = b.val(format!("icmp {cmp} i{bits} {v}, 0"));
            b.branch(cond, direct_target(i).unwrap(), pc + 4);
            return Ok(true);
        }
        "tbz" | "tbnz" => {
            let (r, bits) = b.reg_op(i, 0)?;
            let bit = b.imm_op(i, 1)?;
            let v = b.read(r, bits)?;
            let shifted = b.val(format!("lshr i{bits} {v}, {bit}"));
            let low = b.val(format!("trunc i{bits} {shifted} to i1"));
            let cond = if m == "tbz" { b.not(low) } else { low };
            b.branch(cond, direct_target(i).unwrap(), pc + 4);
            return Ok(true);
        }
        "brk" | "udf" | "hlt" => {
            b.fail(pc, &format!("`{i}` executed"));
            return Ok(true);
        }
        "svc" => b.svc(i.address() + 4),
        // Barriers: a full fence (WebAssembly has only the one). Spin-wait
        // hints keep the compiler from caching guest memory across them, so
        // a loop polling a word another thread writes sees the write.
        "dmb" | "dsb" => b.line("fence seq_cst"),
        "yield" | "wfe" | "isb" => b.line("fence syncscope(\"singlethread\") seq_cst"),
        "nop" | "hint" | "wfi" | "sev" | "sevl" | "csdb" | "clrex"
        | "bti" | "prfm" | "prfum" | "paciasp" | "autiasp" | "pacibsp" | "autibsp" | "xpaclri" | "pacia"
        | "autia1716" | "autib1716" | "pacia1716" | "pacib1716" | "autia" | "pacib" | "autib" | "dc" | "ic" | "dvp"
        | "cfp" | "cpp" => {}

        // --- Element / register moves between files ------------------------------
        "mov" if is_v(ops.first()) || is_v(ops.get(1)) || ops.iter().any(is_elem) => vector_move(b, i)?,
        "ins" | "umov" | "smov" => vector_move(b, i)?,

        // --- Moves and address computation --------------------------------------
        "mov" | "movz" => {
            let d = b.reg_op(i, 0)?;
            let v = b.operand(&ops[1], d.1)?;
            b.write(d.0, v, d.1)?;
        }
        "movn" => {
            let (d, bits) = b.reg_op(i, 0)?;
            let v = b.imm_op(i, 1)?;
            b.write(d, konst(!v, bits), bits)?;
        }
        "movk" => {
            let (d, bits) = b.reg_op(i, 0)?;
            let (imm, shift) = match &ops[1] {
                Operand::Imm32 { imm, shift } | Operand::Imm64 { imm, shift } => {
                    (imm_u64(imm), if let Some(Shift::LSL(s)) = shift { *s } else { 0 })
                }
                other => bail!("movk operand {other:?}"),
            };
            let old = b.read(d, bits)?;
            let kept = b.val(format!("and i{bits} {old}, {}", konst(!(0xffffu64 << shift), bits)));
            let v = b.val(format!("or i{bits} {kept}, {}", konst(imm << shift, bits)));
            b.write(d, v, bits)?;
        }
        "adr" | "adrp" => {
            let (d, _) = b.reg_op(i, 0)?;
            let a = b.imm_op(i, 1)?;
            b.write(d, konst(a, 64), 64)?;
        }
        "mrs" => {
            let (d, _) = b.reg_op(i, 0)?;
            let v = match sysreg(&ops[1])?.as_str() {
                "tpidr_el0" => b.val("load i64, ptr %TP"),
                // Cache line 64 bytes; DC ZVA prohibited (bit 4 set), so
                // musl's memset takes its plain-store path.
                "ctr_el0" => konst(0x8444_c004, 64),
                "dczid_el0" => konst(0x10, 64),
                "fpsr" => {
                    b.flush();
                    let v = b.val("load i32, ptr %FPSR");
                    b.val(format!("zext i32 {v} to i64"))
                }
                "midr_el1" | "fpcr" | "cntfrq_el0" | "cntvct_el0" => konst(0, 64),
                other => bail!("system register {other}"),
            };
            b.write(d, v, 64)?;
        }
        "msr" => {
            let name = sysreg(&ops[0])?;
            let v = b.operand(&ops[1], 64)?;
            match name.as_str() {
                "tpidr_el0" => b.line(format!("store i64 {v}, ptr %TP")),
                "fpsr" => {
                    // Flags raised earlier in the block are overwritten too.
                    b.flush();
                    let v = b.val(format!("trunc i64 {v} to i32"));
                    b.line(format!("store i32 {v}, ptr %FPSR"));
                }
                "fpcr" => {}
                other => bail!("system register {other}"),
            }
        }

        // --- Integer arithmetic --------------------------------------------------
        "add" | "sub" | "adds" | "subs" | "cmp" | "cmn" | "neg" | "negs" | "adc" | "adcs" | "sbc" | "sbcs" | "ngc"
        | "ngcs"
            if !is_v(ops.first()) =>
        {
            let (d, bits, a, bop) = match m {
                "cmp" | "cmn" => {
                    let bits = b.reg_op(i, 0)?.1;
                    let a = b.operand(&ops[0], bits)?;
                    (R::Zr, bits, a, &ops[1])
                }
                "neg" | "negs" | "ngc" | "ngcs" => {
                    let (d, bits) = b.reg_op(i, 0)?;
                    (d, bits, "0".to_string(), &ops[1])
                }
                _ => {
                    let (d, bits) = b.reg_op(i, 0)?;
                    let a = b.operand(&ops[1], bits)?;
                    (d, bits, a, &ops[2])
                }
            };
            let v = b.operand(bop, bits)?;
            let subtract = matches!(m, "sub" | "subs" | "cmp" | "neg" | "negs" | "sbc" | "sbcs" | "ngc" | "ngcs");
            let with_carry = matches!(m, "adc" | "adcs" | "sbc" | "sbcs" | "ngc" | "ngcs");
            let set = matches!(m, "adds" | "subs" | "cmp" | "cmn" | "negs" | "adcs" | "sbcs" | "ngcs");
            let v = if subtract { b.val(format!("xor i{bits} {v}, -1")) } else { v };
            let carry = if with_carry {
                b.get_flag(C)
            } else if subtract {
                "true".into()
            } else {
                "false".into()
            };
            let r = b.add_with_carry(&a, &v, &carry, bits, set);
            b.write(d, r, bits)?;
        }
        "mul" | "madd" | "msub" | "mneg" if !is_v(ops.first()) => {
            let (d, bits) = b.reg_op(i, 0)?;
            let x = b.operand(&ops[1], bits)?;
            let y = b.operand(&ops[2], bits)?;
            let p = b.val(format!("mul i{bits} {x}, {y}"));
            let r = match m {
                "mul" => p,
                "mneg" => b.val(format!("sub i{bits} 0, {p}")),
                _ => {
                    let acc = b.operand(&ops[3], bits)?;
                    let op = if m == "madd" { "add" } else { "sub" };
                    b.val(format!("{op} i{bits} {acc}, {p}"))
                }
            };
            b.write(d, r, bits)?;
        }
        "smull" | "umull" | "smaddl" | "umaddl" | "smsubl" | "umsubl" | "smnegl" | "umnegl" if !is_v(ops.first()) => {
            let (d, _) = b.reg_op(i, 0)?;
            let ext = if m.starts_with('s') { "sext" } else { "zext" };
            let x = b.operand(&ops[1], 32)?;
            let y = b.operand(&ops[2], 32)?;
            let x = b.val(format!("{ext} i32 {x} to i64"));
            let y = b.val(format!("{ext} i32 {y} to i64"));
            let p = b.val(format!("mul i64 {x}, {y}"));
            let r = match &m[1..] {
                "mull" => p,
                "negl" => b.val(format!("sub i64 0, {p}")),
                rest => {
                    let acc = b.operand(&ops[3], 64)?;
                    let op = if rest == "maddl" { "add" } else { "sub" };
                    b.val(format!("{op} i64 {acc}, {p}"))
                }
            };
            b.write(d, r, 64)?;
        }
        "smulh" | "umulh" => {
            let (d, _) = b.reg_op(i, 0)?;
            let ext = if m == "smulh" { "sext" } else { "zext" };
            let x = b.operand(&ops[1], 64)?;
            let y = b.operand(&ops[2], 64)?;
            let x = b.val(format!("{ext} i64 {x} to i128"));
            let y = b.val(format!("{ext} i64 {y} to i128"));
            let p = b.val(format!("mul i128 {x}, {y}"));
            let hi = b.val(format!("lshr i128 {p}, 64"));
            let r = b.val(format!("trunc i128 {hi} to i64"));
            b.write(d, r, 64)?;
        }
        "udiv" | "sdiv" => {
            // Division by zero gives 0; INT_MIN / -1 gives INT_MIN.
            let (d, bits) = b.reg_op(i, 0)?;
            let x = b.operand(&ops[1], bits)?;
            let y = b.operand(&ops[2], bits)?;
            let zero = b.val(format!("icmp eq i{bits} {y}, 0"));
            let r = if m == "sdiv" {
                let neg1 = b.val(format!("icmp eq i{bits} {y}, -1"));
                let bad = b.val(format!("or i1 {zero}, {neg1}"));
                let safe_y = b.val(format!("select i1 {bad}, i{bits} 1, i{bits} {y}"));
                let q = b.val(format!("sdiv i{bits} {x}, {safe_y}"));
                let negx = b.val(format!("sub i{bits} 0, {x}"));
                let q1 = b.val(format!("select i1 {neg1}, i{bits} {negx}, i{bits} {q}"));
                b.val(format!("select i1 {zero}, i{bits} 0, i{bits} {q1}"))
            } else {
                let safe_y = b.val(format!("select i1 {zero}, i{bits} 1, i{bits} {y}"));
                let q = b.val(format!("udiv i{bits} {x}, {safe_y}"));
                b.val(format!("select i1 {zero}, i{bits} 0, i{bits} {q}"))
            };
            b.write(d, r, bits)?;
        }

        // --- Logic --------------------------------------------------------------
        // orr/bic vd.T, #imm{, lsl #s}: the (shifted) immediate in every lane.
        "orr" | "bic"
            if is_v(ops.first()) && matches!(ops.get(1), Some(Operand::Imm32 { .. } | Operand::Imm64 { .. })) =>
        {
            let (d, e, total) = vec_dst(&ops[0])?;
            let imm = b.imm_op(i, 1)? as u128 & ((1u128 << e) - 1);
            let pattern = imm * broadcast(e, total);
            let old = b.read_v(d, 128);
            let r = if m == "orr" {
                b.val(format!("or i128 {old}, {}", konst128(pattern)))
            } else {
                b.val(format!("and i128 {old}, {}", konst128(!pattern)))
            };
            let r = if total < 128 { b.val(format!("and i128 {r}, {}", konst128((1u128 << total) - 1))) } else { r };
            b.write_v(d, r, 128);
        }
        "and" | "orr" | "eor" | "bic" | "orn" | "eon" | "ands" | "bics" | "tst" | "mvn" if is_v(ops.first()) => {
            // Whole-register bitwise operations on vectors.
            let (vd, _, total) = vec_dst(&ops[0])?;
            let x = read_v128(b, &ops[1])?;
            let mut r = if m == "mvn" {
                b.val(format!("xor i128 {x}, -1"))
            } else {
                let y = read_v128(b, &ops[2])?;
                let y = if matches!(m, "bic" | "orn") { b.val(format!("xor i128 {y}, -1")) } else { y };
                let op = match m {
                    "and" | "bic" => "and",
                    "orr" | "orn" => "or",
                    "eor" | "eon" => "xor",
                    _ => bail!("vector {m}"),
                };
                b.val(format!("{op} i128 {x}, {y}"))
            };
            if total < 128 {
                r = b.val(format!("and i128 {r}, {}", konst128((1u128 << total) - 1)));
            }
            b.write_v(vd, r, 128);
        }
        "and" | "orr" | "eor" | "bic" | "orn" | "eon" | "ands" | "bics" | "tst" | "mvn" => {
            let (d, bits, a, bop) = match m {
                "tst" => {
                    let bits = b.reg_op(i, 0)?.1;
                    let a = b.operand(&ops[0], bits)?;
                    (R::Zr, bits, a, &ops[1])
                }
                "mvn" => {
                    let (d, bits) = b.reg_op(i, 0)?;
                    (d, bits, "0".to_string(), &ops[1])
                }
                _ => {
                    let (d, bits) = b.reg_op(i, 0)?;
                    let a = b.operand(&ops[1], bits)?;
                    (d, bits, a, &ops[2])
                }
            };
            let v = b.operand(bop, bits)?;
            let v = if matches!(m, "bic" | "orn" | "eon" | "bics" | "mvn") {
                b.val(format!("xor i{bits} {v}, -1"))
            } else {
                v
            };
            let op = match m {
                "and" | "ands" | "tst" | "bic" | "bics" => "and",
                "orr" | "orn" | "mvn" => "or",
                _ => "xor",
            };
            let r = b.val(format!("{op} i{bits} {a}, {v}"));
            if matches!(m, "ands" | "bics" | "tst") {
                b.logic_flags(&r, bits);
            }
            b.write(d, r, bits)?;
        }

        // --- Shifts, bitfields and bit manipulation ------------------------------------
        "lsl" | "lsr" | "asr" | "ror" | "lslv" | "lsrv" | "asrv" | "rorv" if !is_v(ops.first()) => {
            let (d, bits) = b.reg_op(i, 0)?;
            let x = b.operand(&ops[1], bits)?;
            let amount = match &ops[2] {
                Operand::Reg { .. } => {
                    let r = b.operand(&ops[2], bits)?;
                    b.val(format!("and i{bits} {r}, {}", bits - 1))
                }
                _ => konst(b.imm_op(i, 2)? % bits as u64, bits),
            };
            let r = match &m[..3] {
                "lsl" => b.val(format!("shl i{bits} {x}, {amount}")),
                "lsr" => b.val(format!("lshr i{bits} {x}, {amount}")),
                "asr" => b.val(format!("ashr i{bits} {x}, {amount}")),
                _ => b.val(format!("call i{bits} @llvm.fshr.i{bits}(i{bits} {x}, i{bits} {x}, i{bits} {amount})")),
            };
            b.write(d, r, bits)?;
        }
        "extr" => {
            let (d, bits) = b.reg_op(i, 0)?;
            let hi = b.operand(&ops[1], bits)?;
            let lo = b.operand(&ops[2], bits)?;
            let lsb = b.imm_op(i, 3)?;
            let r = b.val(format!("call i{bits} @llvm.fshr.i{bits}(i{bits} {hi}, i{bits} {lo}, i{bits} {lsb})"));
            b.write(d, r, bits)?;
        }
        "ubfx" | "sbfx" | "ubfiz" | "sbfiz" | "bfi" | "bfxil" | "bfc" => {
            let (d, bits) = b.reg_op(i, 0)?;
            let (src, lsb, width) = if m == "bfc" {
                ("0".to_string(), b.imm_op(i, 1)?, b.imm_op(i, 2)?)
            } else {
                (b.operand(&ops[1], bits)?, b.imm_op(i, 2)?, b.imm_op(i, 3)?)
            };
            let mask = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
            let r = match m {
                "ubfx" | "sbfx" => {
                    let shifted = b.val(format!("lshr i{bits} {src}, {lsb}"));
                    let field = b.val(format!("and i{bits} {shifted}, {}", konst(mask, bits)));
                    if m == "ubfx" {
                        field
                    } else {
                        let up = bits as u64 - width;
                        let l = b.val(format!("shl i{bits} {field}, {up}"));
                        b.val(format!("ashr i{bits} {l}, {up}"))
                    }
                }
                "ubfiz" | "sbfiz" => {
                    let field = b.val(format!("and i{bits} {src}, {}", konst(mask, bits)));
                    let field = if m == "sbfiz" {
                        let up = bits as u64 - width;
                        let l = b.val(format!("shl i{bits} {field}, {up}"));
                        b.val(format!("ashr i{bits} {l}, {up}"))
                    } else {
                        field
                    };
                    b.val(format!("shl i{bits} {field}, {lsb}"))
                }
                "bfi" | "bfc" => {
                    let old = b.read(d, bits)?;
                    let kept = b.val(format!("and i{bits} {old}, {}", konst(!(mask << lsb), bits)));
                    let field = b.val(format!("and i{bits} {src}, {}", konst(mask, bits)));
                    let placed = b.val(format!("shl i{bits} {field}, {lsb}"));
                    b.val(format!("or i{bits} {kept}, {placed}"))
                }
                _ => {
                    // bfxil: insert a field from src's low bits into the low bits.
                    let old = b.read(d, bits)?;
                    let kept = b.val(format!("and i{bits} {old}, {}", konst(!mask, bits)));
                    let shifted = b.val(format!("lshr i{bits} {src}, {lsb}"));
                    let field = b.val(format!("and i{bits} {shifted}, {}", konst(mask, bits)));
                    b.val(format!("or i{bits} {kept}, {field}"))
                }
            };
            b.write(d, r, bits)?;
        }
        "sxtb" | "sxth" | "sxtw" | "uxtb" | "uxth" if !is_v(ops.first()) => {
            let (d, bits) = b.reg_op(i, 0)?;
            let from = match &m[3..] {
                "b" => 8,
                "h" => 16,
                _ => 32,
            };
            let src = b.operand(&ops[1], bits)?;
            let narrow = b.resize(src, bits, from, false);
            let r = b.resize(narrow, from, bits, m.starts_with('s'));
            b.write(d, r, bits)?;
        }
        "rev" | "rev64" | "rev32" | "rev16" if !is_v(ops.first()) => {
            let (d, bits) = b.reg_op(i, 0)?;
            let x = b.operand(&ops[1], bits)?;
            let r = match m {
                "rev16" => {
                    let hi = b.val(format!("lshr i{bits} {x}, 8"));
                    let hi = b.val(format!("and i{bits} {hi}, {}", konst(0x00ff00ff00ff00ff, bits)));
                    let lo = b.val(format!("shl i{bits} {x}, 8"));
                    let lo = b.val(format!("and i{bits} {lo}, {}", konst(0xff00ff00ff00ff00, bits)));
                    b.val(format!("or i{bits} {hi}, {lo}"))
                }
                "rev32" if bits == 64 => {
                    let lo = b.val(format!("trunc i64 {x} to i32"));
                    let hi = b.val(format!("lshr i64 {x}, 32"));
                    let hi = b.val(format!("trunc i64 {hi} to i32"));
                    let lo = b.val(format!("call i32 @llvm.bswap.i32(i32 {lo})"));
                    let hi = b.val(format!("call i32 @llvm.bswap.i32(i32 {hi})"));
                    let lo = b.val(format!("zext i32 {lo} to i64"));
                    let hi = b.val(format!("zext i32 {hi} to i64"));
                    let hi = b.val(format!("shl i64 {hi}, 32"));
                    b.val(format!("or i64 {hi}, {lo}"))
                }
                _ => b.val(format!("call i{bits} @llvm.bswap.i{bits}(i{bits} {x})")),
            };
            b.write(d, r, bits)?;
        }
        "clz" | "cls" | "rbit" if !is_v(ops.first()) => {
            let (d, bits) = b.reg_op(i, 0)?;
            let x = b.operand(&ops[1], bits)?;
            let r = match m {
                "clz" => b.val(format!("call i{bits} @llvm.ctlz.i{bits}(i{bits} {x}, i1 false)")),
                "cls" => {
                    let s = b.val(format!("ashr i{bits} {x}, 1"));
                    let t = b.val(format!("xor i{bits} {x}, {s}"));
                    let c = b.val(format!("call i{bits} @llvm.ctlz.i{bits}(i{bits} {t}, i1 false)"));
                    b.val(format!("sub i{bits} {c}, 1"))
                }
                _ => b.val(format!("call i{bits} @llvm.bitreverse.i{bits}(i{bits} {x})")),
            };
            b.write(d, r, bits)?;
        }

        // --- Conditional operations ------------------------------------------------------
        "csel" | "csinc" | "csinv" | "csneg" | "cset" | "csetm" | "cinc" | "cinv" | "cneg" => {
            let (d, bits) = b.reg_op(i, 0)?;
            let (x, y, cond) = match m {
                "cset" | "csetm" => {
                    let c = b.cond_op(i, 1)?;
                    ("0".to_string(), "0".to_string(), c)
                }
                "cinc" | "cinv" | "cneg" => {
                    // d = cond ? s : f(s): csinc/csinv/csneg with one source
                    // and the condition inverted.
                    let s = b.operand(&ops[1], bits)?;
                    let c = b.cond_op(i, 2)?;
                    let c = b.not(c);
                    (s.clone(), s, c)
                }
                _ => {
                    let x = b.operand(&ops[1], bits)?;
                    let y = b.operand(&ops[2], bits)?;
                    let c = b.cond_op(i, 3)?;
                    (x, y, c)
                }
            };
            let alt = match m {
                "csel" => y,
                "csinc" | "cinc" => b.val(format!("add i{bits} {y}, 1")),
                "csinv" | "cinv" | "csetm" => b.val(format!("xor i{bits} {y}, -1")),
                "csneg" | "cneg" => b.val(format!("sub i{bits} 0, {y}")),
                _ => b.val(format!("add i{bits} {y}, 1")), // cset: false -> 0+1... see below
            };
            // cset/csetm: true -> 1 / -1, false -> 0. Model as select(cond,alt,x).
            let (x, alt) = if matches!(m, "cset" | "csetm") { (alt, x) } else { (x, alt) };
            let r = b.val(format!("select i1 {cond}, i{bits} {x}, i{bits} {alt}"));
            b.write(d, r, bits)?;
        }
        "ccmp" | "ccmn" => {
            let (_, bits) = b.reg_op(i, 0)?;
            let x = b.operand(&ops[0], bits)?;
            let y = b.operand(&ops[1], bits)?;
            let nzcv = b.imm_op(i, 2)?;
            let cond = b.cond_op(i, 3)?;
            let y = if m == "ccmp" { b.val(format!("xor i{bits} {y}, -1")) } else { y };
            let carry = if m == "ccmp" { "true" } else { "false" };
            b.add_with_carry(&x, &y, carry, bits, true);
            let computed: Vec<String> = (0..4).map(|f| b.get_flag(f)).collect();
            let mut merged = Vec::new();
            for (f, c) in computed.iter().enumerate() {
                let imm = if nzcv >> (3 - f) & 1 == 1 { "true" } else { "false" };
                merged.push(b.val(format!("select i1 {cond}, i1 {c}, i1 {imm}")));
            }
            let [n, z, cf, v] = <[String; 4]>::try_from(merged).unwrap();
            b.set_flags(n, z, cf, v);
        }

        // --- Loads and stores --------------------------------------------------------------
        "ldr" | "ldur" | "str" | "stur" | "ldrb" | "ldurb" | "ldrh" | "ldurh" | "strb" | "sturb" | "strh" | "sturh"
        | "ldrsb" | "ldursb" | "ldrsh" | "ldursh" | "ldrsw" | "ldursw" | "ldtr" | "sttr" => {
            let r = b.reg_op(i, 0)?;
            let (mem_bits, signed) = access_width(m, r.1);
            let addr = b.address(&ops[1])?;
            if m.starts_with("st") {
                let v = b.read_any(r)?;
                let v = if matches!(r.0, R::V(_)) { v } else { b.resize(v, r.1, mem_bits, false) };
                b.store(&addr, &v, mem_bits);
            } else if let R::V(n) = r.0 {
                let v = b.load(&addr, mem_bits);
                b.write_v(n, v, mem_bits);
            } else {
                let v = b.load(&addr, mem_bits);
                let v = b.resize(v, mem_bits, r.1, signed);
                b.write(r.0, v, r.1)?;
            }
        }
        // Acquire loads and release stores: atomic accesses. A load-exclusive
        // also records the value it read in the monitor (see "stxr").
        "ldar" | "ldarb" | "ldarh" | "ldapr" | "ldaprb" | "ldaprh" | "ldlar" | "ldlarb" | "ldlarh" | "ldaxr"
        | "ldaxrb" | "ldaxrh" | "ldxr" | "ldxrb" | "ldxrh" => {
            let r = b.reg_op(i, 0)?;
            let (mem_bits, _) = access_width(m, r.1);
            let addr = b.address(&ops[1])?;
            let v = b.load_atomic(&addr, mem_bits);
            if m.contains("xr") {
                let wide = b.resize(v.clone(), mem_bits, 64, false);
                b.line(format!("store i64 {wide}, ptr %MONITOR{REG}"));
            }
            let v = b.resize(v, mem_bits, r.1, false);
            b.write(r.0, v, r.1)?;
        }
        "stlr" | "stlrb" | "stlrh" | "stllr" | "stllrb" | "stllrh" => {
            let r = b.reg_op(i, 0)?;
            let (mem_bits, _) = access_width(m, r.1);
            let addr = b.address(&ops[1])?;
            let v = b.read(r.0, r.1)?;
            let v = b.resize(v, r.1, mem_bits, false);
            b.store_atomic(&addr, &v, mem_bits);
        }
        // Store-exclusive, as a compare-and-swap against the value the
        // load-exclusive read: it succeeds (status 0) if memory still holds
        // that value, which is how QEMU emulates the exclusive monitor. (A
        // write of the same value in between goes unnoticed, which the
        // lock-free algorithms compilers emit tolerate.)
        "stxr" | "stlxr" | "stxrb" | "stlxrb" | "stxrh" | "stlxrh" => {
            let status = b.reg_op(i, 0)?;
            let r = b.reg_op(i, 1)?;
            let (mem_bits, _) = access_width(m, r.1);
            let addr = b.address(&ops[2])?;
            let v = b.read(r.0, r.1)?;
            let v = b.resize(v, r.1, mem_bits, false);
            let seen = b.val(format!("load i64, ptr %MONITOR{REG}"));
            let seen = b.resize(seen, 64, mem_bits, false);
            let (_, ok) = b.cmpxchg(&addr, &seen, &v, mem_bits);
            let failed = b.val(format!("xor i1 {ok}, true"));
            let failed = b.val(format!("zext i1 {failed} to i{}", status.1));
            b.write(status.0, failed, status.1)?;
        }
        "ldp" | "stp" | "ldnp" | "stnp" | "ldpsw" => {
            let r1 = b.reg_op(i, 0)?;
            let r2 = b.reg_op(i, 1)?;
            let (mem_bits, signed) = if m == "ldpsw" { (32, true) } else { (r1.1, false) };
            let addr = b.address(&ops[2])?;
            let addr2 = b.val(format!("add i64 {addr}, {}", mem_bits / 8));
            for (r, a) in [(r1, addr), (r2, addr2)] {
                if m.starts_with("st") {
                    let v = b.read_any(r)?;
                    b.store(&a, &v, mem_bits);
                } else if let R::V(n) = r.0 {
                    let v = b.load(&a, mem_bits);
                    b.write_v(n, v, mem_bits);
                } else {
                    let v = b.load(&a, mem_bits);
                    let v = b.resize(v, mem_bits, r.1, signed);
                    b.write(r.0, v, r.1)?;
                }
            }
        }
        // Single-lane structure load/store (`ld2 {v0.s, v1.s}[1], [x0]`):
        // lane k of register r is at addr + r * element size; other lanes
        // are kept.
        "ld1" | "ld2" | "ld3" | "ld4" | "st1" | "st2" | "st3" | "st4" if list_lane(&ops[0]).is_some() => {
            let k = list_lane(&ops[0]).unwrap();
            let (regs, elem, _) = reg_list(&ops[0])?;
            let addr = structure_address(b, i)?;
            for (r, &n) in regs.iter().enumerate() {
                let a = b.val(format!("add i64 {addr}, {}", r as u32 * elem / 8));
                if m.starts_with("ld") {
                    let v = b.load(&a, elem);
                    insert_lane(b, n, elem, k, v);
                } else {
                    let v = b.read_v(n, 128);
                    let lane = vlane(b, &v, elem, k);
                    b.store(&a, &lane, elem);
                }
            }
        }
        // Vector load/store of a single register (`ld1 {v0.16b}, [x0]`).
        "ld1" | "st1" if reg_list(&ops[0])?.0.len() > 1 => {
            // Several consecutive registers: register r at addr + r * size.
            let (regs, _, total) = reg_list(&ops[0])?;
            let addr = structure_address(b, i)?;
            for (r, &n) in regs.iter().enumerate() {
                let a = b.val(format!("add i64 {addr}, {}", r as u32 * total / 8));
                if m == "ld1" {
                    let v = b.load(&a, total);
                    b.write_v(n, v, total);
                } else {
                    let v = b.read_v(n, total);
                    b.store(&a, &v, total);
                }
            }
        }
        "ld2" | "ld3" | "ld4" | "st2" | "st3" | "st4" => {
            // Interleaved structures: element k of register r is at
            // addr + (k * N + r) * element size.
            // One vector load or store of all N registers' elements, split or
            // interleaved with shuffles.
            let (regs, elem, total) = reg_list(&ops[0])?;
            let addr = structure_address(b, i)?;
            let n = regs.len() as u32;
            let lanes = nlanes(total, elem);
            let all = format!("<{} x i{elem}>", n * lanes);
            let one = format!("<{lanes} x i{elem}>");
            let p = b.val(format!("inttoptr i64 {addr} to ptr"));
            if m.starts_with("ld") {
                let v = b.val(format!("load {all}, ptr {p}, align 1{}", super::MEM));
                let values: Vec<String> = (0..n)
                    .map(|r| {
                        let mask: Vec<String> = (0..lanes).map(|k| format!("i32 {}", k * n + r)).collect();
                        let reg = b.val(format!("shufflevector {all} {v}, {all} poison, <{lanes} x i32> <{}>", mask.join(", ")));
                        let bits = b.val(format!("bitcast {one} {reg} to i{total}"));
                        if total == 128 { bits } else { b.val(format!("zext i{total} {bits} to i128")) }
                    })
                    .collect();
                for (&reg, v) in regs.iter().zip(values) {
                    b.write_v(reg, v, 128);
                }
            } else {
                let mut acc = "poison".to_string();
                for (r, &reg) in regs.iter().enumerate() {
                    let v = b.read_v(reg, 128);
                    for k in 0..lanes {
                        let lane = vlane(b, &v, elem, k);
                        acc = b.val(format!("insertelement {all} {acc}, i{elem} {lane}, i32 {}", k * n + r as u32));
                    }
                }
                b.line(format!("store {all} {acc}, ptr {p}, align 1{}", super::MEM));
            }
        }
        "ld1" | "st1" | "ld1r" => {
            let list = &ops[0];
            let (regs, elem, total) = reg_list(list)?;
            let addr = structure_address(b, i)?;
            if regs.len() != 1 {
                bail!("{m} with {} registers", regs.len());
            }
            if m == "ld1r" {
                // Load one element and replicate it across the register.
                let v = b.load(&addr, elem);
                let wide = b.val(format!("zext i{elem} {v} to i128"));
                let r = b.val(format!("mul i128 {wide}, {}", konst128(broadcast(elem, total))));
                b.write_v(regs[0], r, 128);
            } else if m == "ld1" {
                let v = b.load(&addr, total);
                b.write_v(regs[0], v, total);
            } else {
                let v = b.read_v(regs[0], total);
                b.store(&addr, &v, total);
            }
        }

        // --- SIMD/FP moves and immediates ------------------------------------------------------
        "fmov" => fmov(b, i)?,
        "dup" | "movi" | "mvni" if is_v(ops.first()) && !is_elem(&ops[1]) => {
            let (d, e, total) = vec_dst(&ops[0])?;
            let elem = match m {
                "dup" => {
                    let s = b.reg_op(i, 1)?;
                    let v = b.read(s.0, s.1)?;
                    b.resize(v, s.1, e, false)
                }
                _ => {
                    let v = b.imm_op(i, 1)?;
                    konst(if m == "mvni" { !v } else { v }, e)
                }
            };
            let wide = b.val(format!("zext i{e} {elem} to i128"));
            let r = b.val(format!("mul i128 {wide}, {}", konst128(broadcast(e, total))));
            b.write_v(d, r, 128);
        }
        "dup" if is_v(ops.first()) => {
            // Element form: dup vd.T, vn.ts[k].
            let (d, e, total) = vec_dst(&ops[0])?;
            let (sn, _, k) = elem_of(&ops[1]).ok_or_else(|| anyhow::anyhow!("dup source"))?;
            let sv = b.read_v(sn, 128);
            let lane = vlane(b, &sv, e, k);
            let wide = b.val(format!("zext i{e} {lane} to i128"));
            let r = b.val(format!("mul i128 {wide}, {}", konst128(broadcast(e, total))));
            b.write_v(d, r, 128);
        }

        // --- SIMD integer, element-wise -----------------------------------------------------
        // Vector, or scalar on a D register (add d0, d1, d2): one 64-bit lane.
        "add" | "sub" | "mul" if is_v(ops.first()) => vector_binary(b, i, m)?,
        "saddw" | "uaddw" | "ssubw" | "usubw" | "saddw2" | "uaddw2" | "ssubw2" | "usubw2" | "saddl" | "uaddl"
        | "ssubl" | "usubl" | "saddl2" | "uaddl2" | "ssubl2" | "usubl2" => {
            // Wide (…w: n is already wide) or long (…l: both widen): the
            // narrow operand's low half, or high half for the "2" forms.
            let (d, e, _) = vec_dst(&ops[0])?;
            let x = read_v128(b, &ops[1])?;
            let y = read_v128(b, &ops[2])?;
            let lanes = 128 / e;
            let first = if m.ends_with('2') { lanes } else { 0 };
            let ext = if m.starts_with('s') { "sext" } else { "zext" };
            let long = m.trim_end_matches('2').ends_with('l');
            let mut parts = Vec::new();
            for k in 0..lanes {
                let xk = if long {
                    let n = vlane(b, &x, e / 2, first + k);
                    b.val(format!("{ext} i{} {n} to i{e}", e / 2))
                } else {
                    vlane(b, &x, e, k)
                };
                let yn = vlane(b, &y, e / 2, first + k);
                let yk = b.val(format!("{ext} i{} {yn} to i{e}", e / 2));
                let op = if m[1..].starts_with("add") { "add" } else { "sub" };
                parts.push(b.val(format!("{op} i{e} {xk}, {yk}")));
            }
            let r = vbuild(b, e, &parts);
            b.write_v(d, r, 128);
        }
        "uaddlp" | "saddlp" | "uadalp" | "sadalp" => {
            // Pairwise add long, optionally accumulating into the destination.
            let (d, e, total) = vec_dst(&ops[0])?;
            let x = read_v128(b, &ops[1])?;
            let old = b.read_v(d, 128);
            let ext = if m.starts_with('s') { "sext" } else { "zext" };
            let mut parts = Vec::new();
            for k in 0..nlanes(total, e) {
                let lo = vlane(b, &x, e / 2, 2 * k);
                let hi = vlane(b, &x, e / 2, 2 * k + 1);
                let lo = b.val(format!("{ext} i{} {lo} to i{e}", e / 2));
                let hi = b.val(format!("{ext} i{} {hi} to i{e}", e / 2));
                let mut s = b.val(format!("add i{e} {lo}, {hi}"));
                if m.contains("adalp") {
                    let acc = vlane(b, &old, e, k);
                    s = b.val(format!("add i{e} {s}, {acc}"));
                }
                parts.push(s);
            }
            let r = vbuild(b, e, &parts);
            b.write_v(d, r, 128);
        }
        "cmeq" | "cmgt" | "cmge" | "cmle" | "cmlt" | "cmhi" | "cmhs" | "cmtst" | "umin" | "umax" | "smin" | "smax"
        | "uabd" | "sabd"
            if is_v(ops.first()) =>
        {
            vector_binary(b, i, m)?
        }
        "not" | "abs" | "cnt" | "neg" | "rbit" if is_v(ops.first()) => vector_unary(b, i, m)?,
        "addp" | "umaxp" | "uminp" | "smaxp" | "sminp" if is_v(ops.first()) => {
            let (d, elem, total) = vec_dst(&ops[0])?;
            if ops.len() == 3 {
                // Vector pairwise: concatenate vn:vm, combine adjacent pairs.
                let x = read_v128(b, &ops[1])?;
                let y = read_v128(b, &ops[2])?;
                let half = nlanes(total, elem) / 2;
                let mut parts = Vec::new();
                for src in [&x, &y] {
                    for k in 0..half {
                        let a = vlane(b, src, elem, 2 * k);
                        let c = vlane(b, src, elem, 2 * k + 1);
                        parts.push(match m {
                            "addp" => b.val(format!("add i{elem} {a}, {c}")),
                            _ => {
                                let f = &m[..4]; // umax, umin, smax, smin
                                b.val(format!("call i{elem} @llvm.{f}.i{elem}(i{elem} {a}, i{elem} {c})"))
                            }
                        });
                    }
                }
                let r = vbuild(b, elem, &parts);
                b.write_v(d, r, 128);
            } else {
                // Scalar pairwise (addp d, vn.2d): add the two lanes.
                let (_, e, _) = vec_dst(&ops[1])?;
                let x = read_v128(b, &ops[1])?;
                let a = vlane(b, &x, e, 0);
                let c = vlane(b, &x, e, 1);
                let r = b.val(format!("add i{e} {a}, {c}"));
                b.write_v(d, r, e);
            }
        }
        "shl" | "sshr" | "ushr" | "sshl" | "ushl" if is_v(ops.first()) => vector_shift(b, i, m)?,
        "addv" | "uaddlv" | "saddlv" | "umaxv" | "uminv" | "smaxv" | "sminv" => vector_reduce(b, i, m)?,
        "uzp1" | "uzp2" | "zip1" | "zip2" | "trn1" | "trn2" => vector_permute(b, i, m)?,
        "ext" if is_v(ops.first()) => {
            let (d, _, total) = vec_dst(&ops[0])?;
            let x = read_v128(b, &ops[1])?;
            let y = read_v128(b, &ops[2])?;
            let index = b.imm_op(i, 3)? * 8; // byte index -> bit shift
            // result = (concat(y, x) >> index) truncated to `total` bits.
            let lo = b.val(format!("lshr i128 {x}, {index}"));
            let hi = if index == 0 { "0".to_string() } else { b.val(format!("shl i128 {y}, {}", 128 - index)) };
            let mut r = b.val(format!("or i128 {lo}, {hi}"));
            if total < 128 {
                r = b.val(format!("and i128 {r}, {}", konst128((1u128 << total) - 1)));
            }
            b.write_v(d, r, 128);
        }
        "shrn" | "shrn2" | "xtn" | "xtn2" => vector_narrow(b, i, m)?,

        // --- Scalar floating point ---------------------------------------------------------
        "fadd" | "fsub" | "fmul" | "fdiv" | "fnmul" | "fmin" | "fmax" | "fminnm" | "fmaxnm" if is_scalar_fp(ops) => {
            let (d, bits) = fp_reg(&ops[0])?;
            let ty = fp_ty(bits)?.0;
            let x = read_fp(b, &ops[1], bits)?;
            let y = read_fp(b, &ops[2], bits)?;
            let r = match m {
                "fadd" => b.val(format!("fadd {ty} {x}, {y}")),
                "fsub" => b.val(format!("fsub {ty} {x}, {y}")),
                "fmul" => b.val(format!("fmul {ty} {x}, {y}")),
                "fdiv" => b.val(format!("fdiv {ty} {x}, {y}")),
                "fnmul" => {
                    let p = b.val(format!("fmul {ty} {x}, {y}"));
                    b.val(format!("fneg {ty} {p}"))
                }
                _ => fp_minmax(b, m, ty, &x, &y),
            };
            if !m.contains("min") && !m.contains("max") {
                fp_exceptions(b, ty, &[&x, &y], &r, m == "fdiv");
            }
            write_fp(b, d, r, bits)?;
        }
        "fabs" | "fneg" | "fsqrt" if is_scalar_fp(ops) && ops.len() == 2 && is_v(ops.get(1)) => {
            let (d, bits) = fp_reg(&ops[0])?;
            let (ty, s) = fp_ty(bits)?;
            let x = read_fp(b, &ops[1], bits)?;
            let r = match m {
                "fabs" => b.val(format!("call {ty} @llvm.fabs.{s}({ty} {x})")),
                "fneg" => b.val(format!("fneg {ty} {x}")),
                _ => b.val(format!("call {ty} @llvm.sqrt.{s}({ty} {x})")),
            };
            if m == "fsqrt" {
                fp_exceptions(b, ty, &[&x], &r, false);
            }
            write_fp(b, d, r, bits)?;
        }
        "fmadd" | "fmsub" | "fnmadd" | "fnmsub" => {
            // Fused, one rounding (see `fused`): fmadd a + x*y, fmsub a - x*y,
            // fnmadd -a - x*y, fnmsub -a + x*y.
            let (d, bits) = fp_reg(&ops[0])?;
            let ty = fp_ty(bits)?.0;
            let x = read_fp(b, &ops[1], bits)?;
            let y = read_fp(b, &ops[2], bits)?;
            let a = read_fp(b, &ops[3], bits)?;
            let x = if matches!(m, "fmsub" | "fnmadd") { b.val(format!("fneg {ty} {x}")) } else { x };
            let a = if matches!(m, "fnmadd" | "fnmsub") { b.val(format!("fneg {ty} {a}")) } else { a };
            let r = fused(b, ty, &x, &y, &a);
            fp_exceptions(b, ty, &[&x, &y, &a], &r, false);
            write_fp(b, d, r, bits)?;
        }
        "fcmp" | "fcmpe" => {
            let (_, bits) = fp_reg(&ops[0])?;
            let (ty, _) = fp_ty(bits)?;
            let x = read_fp(b, &ops[0], bits)?;
            let y = match ops.get(1) {
                Some(o) if is_v(Some(o)) => read_fp(b, o, bits)?,
                _ => format!("{ty} 0.0"),
            };
            let y = y.strip_prefix(&format!("{ty} ")).map(String::from).unwrap_or(y);
            fp_compare(b, ty, &x, &y);
        }
        "fccmp" | "fccmpe" => {
            let (_, bits) = fp_reg(&ops[0])?;
            let (ty, _) = fp_ty(bits)?;
            let x = read_fp(b, &ops[0], bits)?;
            let y = read_fp(b, &ops[1], bits)?;
            let nzcv = b.imm_op(i, 2)?;
            let cond = b.cond_op(i, 3)?;
            fp_compare(b, ty, &x, &y);
            let computed: Vec<String> = (0..4).map(|f| b.get_flag(f)).collect();
            let mut merged = Vec::new();
            for (f, c) in computed.iter().enumerate() {
                let imm = if nzcv >> (3 - f) & 1 == 1 { "true" } else { "false" };
                merged.push(b.val(format!("select i1 {cond}, i1 {c}, i1 {imm}")));
            }
            let [n, z, cf, v] = <[String; 4]>::try_from(merged).unwrap();
            b.set_flags(n, z, cf, v);
        }
        "fcsel" => {
            let (d, bits) = fp_reg(&ops[0])?;
            let ty = fp_ty(bits)?.0;
            let x = read_fp(b, &ops[1], bits)?;
            let y = read_fp(b, &ops[2], bits)?;
            let cond = b.cond_op(i, 3)?;
            let r = b.val(format!("select i1 {cond}, {ty} {x}, {ty} {y}"));
            write_fp(b, d, r, bits)?;
        }
        // --- More SIMD floating point: fused multiply-add, compares, pairwise
        // and across-vector reductions. -------------------------------------------------
        "fmla" | "fmls" if is_v(ops.first()) && matches!(vec_dst(&ops[0])?.1, 32 | 64) => {
            // Vector, by element, or scalar by element (fmla d0, d1, v2.d[1]),
            // fused, as one vector operation: d + x*y, or d - x*y as
            // d + (-x)*y.
            let (d, elem, total) = vec_dst(&ops[0])?;
            let ty = fp_ty(elem)?.0;
            let acc = b.read_v(d, 128);
            let (acc, vt) = fp_vec_in(b, &acc, elem, total)?;
            let x = read_v128(b, &ops[1])?;
            let (x, _) = fp_vec_in(b, &x, elem, total)?;
            let y = fp_second_vec(b, &ops[2], elem, total)?;
            let x = if m == "fmls" { b.val(format!("fneg {vt} {x}")) } else { x };
            let s = if elem == 64 { "v2f64" } else { "v4f32" };
            let r = b.val(format!("call {vt} @llvm.wasm.relaxed.madd.{s}({vt} {x}, {vt} {y}, {vt} {acc})"));
            b.check_fp_vec(ty, 128 / elem, &[&x, &y, &acc], &r, false);
            let r = b.val(format!("bitcast {vt} {r} to i128"));
            b.write_v(d, r, 128);
        }
        // (A 64-bit fdiv stays lane by lane: its zeroed upper lanes would
        // divide 0 by 0 and raise a spurious invalid flag.)
        "fadd" | "fsub" | "fmul" | "fdiv" | "fmax" | "fmin" | "fmaxnm" | "fminnm"
            if has_arrspec(&ops[0])
                && matches!(vec_dst(&ops[0])?.1, 32 | 64)
                && (m != "fdiv" || vec_dst(&ops[0])?.2 == 128) =>
        {
            let (d, elem, total) = vec_dst(&ops[0])?;
            let ty = fp_ty(elem)?.0;
            let x = read_v128(b, &ops[1])?;
            let (x, vt) = fp_vec_in(b, &x, elem, total)?;
            let y = fp_second_vec(b, &ops[2], elem, total)?;
            let r = match m {
                "fmax" | "fmin" | "fmaxnm" | "fminnm" => fp_minmax(b, m, &vt, &x, &y),
                _ => {
                    let r = b.val(format!("{m} {vt} {x}, {y}"));
                    b.check_fp_vec(ty, 128 / elem, &[&x, &y], &r, m == "fdiv");
                    r
                }
            };
            let r = b.val(format!("bitcast {vt} {r} to i128"));
            b.write_v(d, r, 128);
        }
        "fmla" | "fmls" if is_v(ops.first()) => {
            // Half precision: lane by lane, fused: d + x*y, or d - x*y as
            // d + (-x)*y.
            let (d, elem, total) = vec_dst(&ops[0])?;
            let ty = fp_ty(elem)?.0;
            let acc = b.read_v(d, 128);
            let x = read_v128(b, &ops[1])?;
            let (y, ylane) = second_operand(b, &ops[2])?;
            let mut parts = Vec::new();
            for k in 0..nlanes(total, elem) {
                let [af, xf, yf] = [(&acc, k), (&x, k), (&y, ylane.unwrap_or(k))].map(|(v, j)| {
                    let l = vlane(b, v, elem, j);
                    b.val(format!("bitcast i{elem} {l} to {ty}"))
                });
                let xf = if m == "fmls" { b.val(format!("fneg {ty} {xf}")) } else { xf };
                let r = fused(b, ty, &xf, &yf, &af);
                fp_exceptions(b, ty, &[&xf, &yf, &af], &r, false);
                parts.push(b.val(format!("bitcast {ty} {r} to i{elem}")));
            }
            let r = vbuild(b, elem, &parts);
            b.write_v(d, r, 128);
        }
        "fcmeq" | "fcmge" | "fcmgt" | "fcmle" | "fcmlt" | "facge" | "facgt" if is_v(ops.first()) => {
            // All ones where the comparison holds; against a register or #0.0.
            let (d, elem, total) = vec_dst(&ops[0])?;
            let (ty, s) = fp_ty(elem)?;
            let x = read_v128(b, &ops[1])?;
            let y = match ops.get(2) {
                Some(op @ Operand::Reg { .. }) => Some(read_v128(b, op)?),
                _ => None,
            };
            let pred = match m {
                "fcmeq" => "oeq",
                "fcmge" | "facge" => "oge",
                "fcmgt" | "facgt" => "ogt",
                "fcmle" => "ole",
                _ => "olt",
            };
            let mut parts = Vec::new();
            for k in 0..nlanes(total, elem) {
                let xl = vlane(b, &x, elem, k);
                let mut xf = b.val(format!("bitcast i{elem} {xl} to {ty}"));
                let mut yf = match &y {
                    Some(y) => {
                        let yl = vlane(b, y, elem, k);
                        b.val(format!("bitcast i{elem} {yl} to {ty}"))
                    }
                    None => "0.0".to_string(),
                };
                if m.starts_with("fac") {
                    xf = b.val(format!("call {ty} @llvm.fabs.{s}({ty} {xf})"));
                    yf = b.val(format!("call {ty} @llvm.fabs.{s}({ty} {yf})"));
                }
                let c = b.fcmp(pred, ty, &xf, &yf);
                parts.push(b.val(format!("sext i1 {c} to i{elem}")));
            }
            let r = vbuild(b, elem, &parts);
            b.write_v(d, r, 128);
        }
        "faddp" | "fmaxp" | "fminp" | "fmaxnmp" | "fminnmp" if is_v(ops.first()) => {
            let combine = |b: &mut Block, ty: &str, p: &str, q: &str| match m {
                "faddp" => b.val(format!("fadd {ty} {p}, {q}")),
                _ => fp_minmax(b, m, ty, p, q),
            };
            if ops.len() == 3 {
                // Vector: pairs of the concatenation vn:vm.
                let (d, elem, total) = vec_dst(&ops[0])?;
                let ty = fp_ty(elem)?.0;
                let x = read_v128(b, &ops[1])?;
                let y = read_v128(b, &ops[2])?;
                let mut parts = Vec::new();
                for src in [&x, &y] {
                    for k in 0..nlanes(total, elem) / 2 {
                        let [p, q] = [2 * k, 2 * k + 1].map(|j| {
                            let l = vlane(b, src, elem, j);
                            b.val(format!("bitcast i{elem} {l} to {ty}"))
                        });
                        let r = combine(b, ty, &p, &q);
                        parts.push(b.val(format!("bitcast {ty} {r} to i{elem}")));
                    }
                }
                let r = vbuild(b, elem, &parts);
                b.write_v(d, r, 128);
            } else {
                // Scalar: the two lanes of the source (faddp d0, v1.2d).
                let (d, bits) = fp_reg(&ops[0])?;
                let ty = fp_ty(bits)?.0;
                let x = read_v128(b, &ops[1])?;
                let [p, q] = [0, 1].map(|j| {
                    let l = vlane(b, &x, bits, j);
                    b.val(format!("bitcast i{bits} {l} to {ty}"))
                });
                let r = combine(b, ty, &p, &q);
                write_fp(b, d, r, bits)?;
            }
        }
        "fmaxv" | "fminv" | "fmaxnmv" | "fminnmv" => {
            let (d, bits) = fp_reg(&ops[0])?;
            let ty = fp_ty(bits)?.0;
            let (_, elem, total) = vec_dst(&ops[1])?;
            let x = read_v128(b, &ops[1])?;
            let lanes: Vec<String> = (0..nlanes(total, elem))
                .map(|k| {
                    let l = vlane(b, &x, elem, k);
                    b.val(format!("bitcast i{elem} {l} to {ty}"))
                })
                .collect();
            let mut acc = lanes[0].clone();
            for l in &lanes[1..] {
                acc = fp_minmax(b, m, ty, &acc, l);
            }
            write_fp(b, d, acc, bits)?;
        }

        // --- More SIMD integer: multiply-accumulate, shift-right-accumulate,
        // saturating narrows. -----------------------------------------------------------
        "mla" | "mls" if is_v(ops.first()) => {
            let (d, elem, total) = vec_dst(&ops[0])?;
            let acc = b.read_v(d, 128);
            let x = read_v128(b, &ops[1])?;
            let (y, ylane) = second_operand(b, &ops[2])?;
            let mut parts = Vec::new();
            for k in 0..nlanes(total, elem) {
                let a = vlane(b, &acc, elem, k);
                let xk = vlane(b, &x, elem, k);
                let yk = vlane(b, &y, elem, ylane.unwrap_or(k));
                let p = b.val(format!("mul i{elem} {xk}, {yk}"));
                parts.push(b.val(format!("{} i{elem} {a}, {p}", if m == "mla" { "add" } else { "sub" })));
            }
            let r = vbuild(b, elem, &parts);
            b.write_v(d, r, 128);
        }
        "usra" | "ssra" => {
            let (d, elem, total) = vec_dst(&ops[0])?;
            let s = b.imm_op(i, 2)?.min(elem as u64 - 1 + u64::from(m == "usra"));
            let acc = b.read_v(d, 128);
            let x = read_v128(b, &ops[1])?;
            let mut parts = Vec::new();
            for k in 0..nlanes(total, elem) {
                let a = vlane(b, &acc, elem, k);
                let xk = vlane(b, &x, elem, k);
                // A shift by the element size (allowed for usra) gives 0.
                let sh = if s as u32 >= elem {
                    "0".to_string()
                } else {
                    b.val(format!("{} i{elem} {xk}, {s}", if m == "usra" { "lshr" } else { "ashr" }))
                };
                parts.push(b.val(format!("add i{elem} {a}, {sh}")));
            }
            let r = vbuild(b, elem, &parts);
            b.write_v(d, r, 128);
        }
        "sqxtn" | "uqxtn" | "sqxtun" | "sqxtn2" | "uqxtn2" | "sqxtun2" => {
            // Narrow each lane with saturation; the "2" forms fill the upper
            // half and keep the lower. (FPSR.QC isn't modelled.)
            let (d, e, _) = vec_dst(&ops[0])?;
            let (_, wide, src_total) = vec_dst(&ops[1])?;
            let x = read_v128(b, &ops[1])?;
            let old = b.read_v(d, 128);
            let n = src_total / wide;
            let (lo, hi): (i128, i128) = match m.trim_end_matches('2') {
                "sqxtn" => (-(1i128 << (e - 1)), (1i128 << (e - 1)) - 1),
                _ => (0, (1i128 << e) - 1),
            };
            let src_signed = m.starts_with("sq");
            let mut parts = Vec::new();
            if m.ends_with('2') {
                for k in 0..n {
                    parts.push(vlane(b, &old, e, k));
                }
            }
            for k in 0..n {
                let v = vlane(b, &x, wide, k);
                let (gt, lt) = if src_signed { ("sgt", "slt") } else { ("ugt", "ult") };
                let too_big = b.val(format!("icmp {gt} i{wide} {v}, {hi}"));
                let v = b.val(format!("select i1 {too_big}, i{wide} {hi}, i{wide} {v}"));
                let v = if lo < 0 || src_signed {
                    let too_small = b.val(format!("icmp {lt} i{wide} {v}, {lo}"));
                    b.val(format!("select i1 {too_small}, i{wide} {lo}, i{wide} {v}"))
                } else {
                    v
                };
                parts.push(b.val(format!("trunc i{wide} {v} to i{e}")));
            }
            let r = vbuild(b, e, &parts);
            b.write_v(d, r, 128);
        }
        "fcvtl" | "fcvtl2" => {
            // Widen the low (high, for fcvtl2) half's floats: .2s -> .2d.
            let (d, e, _) = vec_dst(&ops[0])?;
            let (wide, narrow) = (fp_ty(e)?.0, fp_ty(e / 2)?.0);
            let x = read_v128(b, &ops[1])?;
            let n = 128 / e;
            let first = if m.ends_with('2') { n } else { 0 };
            let mut parts = Vec::new();
            for k in 0..n {
                let lane = vlane(b, &x, e / 2, first + k);
                let f = b.val(format!("bitcast i{} {lane} to {narrow}", e / 2));
                let w = b.val(format!("fpext {narrow} {f} to {wide}"));
                parts.push(b.val(format!("bitcast {wide} {w} to i{e}")));
            }
            let r = vbuild(b, e, &parts);
            b.write_v(d, r, 128);
        }
        "fcvtn" | "fcvtn2" => {
            // Narrow .2d -> .2s, into the low half (fcvtn2: the high half,
            // keeping the low).
            let (d, e, _) = vec_dst(&ops[0])?;
            let (wide, narrow) = (fp_ty(e * 2)?.0, fp_ty(e)?.0);
            let x = read_v128(b, &ops[1])?;
            let old = b.read_v(d, 128);
            let n = 64 / e;
            let mut parts = Vec::new();
            if m.ends_with('2') {
                for k in 0..n {
                    parts.push(vlane(b, &old, e, k));
                }
            }
            for k in 0..n {
                let lane = vlane(b, &x, e * 2, k);
                let f = b.val(format!("bitcast i{} {lane} to {wide}", e * 2));
                let t = b.val(format!("fptrunc {wide} {f} to {narrow}"));
                let back = b.val(format!("fpext {narrow} {t} to {wide}"));
                fp_exceptions(b, wide, &[&f], &back, false);
                parts.push(b.val(format!("bitcast {narrow} {t} to i{e}")));
            }
            let r = vbuild(b, e, &parts);
            b.write_v(d, r, 128);
        }
        "sli" | "sri" => {
            // Shift each lane and insert, keeping the destination's bits that
            // the shift vacates.
            let (d, e, total) = vec_dst(&ops[0])?;
            let s = b.imm_op(i, 2)? as u32;
            let x = read_v128(b, &ops[1])?;
            let old = b.read_v(d, 128);
            let lane_mask = if e == 64 { u64::MAX } else { (1u64 << e) - 1 };
            let keep = if m == "sli" {
                !(lane_mask << s) & lane_mask
            } else if s >= e {
                lane_mask
            } else {
                !(lane_mask >> s) & lane_mask
            };
            let mut parts = Vec::new();
            for k in 0..nlanes(total, e) {
                let xk = vlane(b, &x, e, k);
                let dk = vlane(b, &old, e, k);
                let shifted = match (m, s >= e) {
                    ("sri", true) => "0".to_string(),
                    ("sli", _) => b.val(format!("shl i{e} {xk}, {s}")),
                    _ => b.val(format!("lshr i{e} {xk}, {s}")),
                };
                let kept = b.val(format!("and i{e} {dk}, {}", konst(keep, e)));
                parts.push(b.val(format!("or i{e} {kept}, {shifted}")));
            }
            let r = vbuild(b, e, &parts);
            b.write_v(d, r, 128);
        }
        "umull" | "smull" | "umull2" | "smull2" | "umlal" | "smlal" | "umlal2" | "smlal2" | "umlsl" | "smlsl"
        | "umlsl2" | "smlsl2"
            if is_v(ops.first()) =>
        {
            // Widening multiply (and accumulate/subtract): the narrow
            // operands' low (or high, "2") half; the second may be an element.
            let (d, e, _) = vec_dst(&ops[0])?;
            let x = read_v128(b, &ops[1])?;
            let n = 128 / e;
            let first = if m.ends_with('2') { n } else { 0 };
            let ext = if m.starts_with('s') { "sext" } else { "zext" };
            let element = elem_of(&ops[2]);
            let y = match element {
                Some((sn, _, _)) => b.read_v(sn, 128),
                None => read_v128(b, &ops[2])?,
            };
            let old = b.read_v(d, 128);
            let mut parts = Vec::new();
            for k in 0..n {
                let xn = vlane(b, &x, e / 2, first + k);
                let yn = match element {
                    Some((_, ee, lane)) => vlane(b, &y, ee, lane),
                    None => vlane(b, &y, e / 2, first + k),
                };
                let xw = b.val(format!("{ext} i{} {xn} to i{e}", e / 2));
                let yw = b.val(format!("{ext} i{} {yn} to i{e}", e / 2));
                let p = b.val(format!("mul i{e} {xw}, {yw}"));
                let acc = vlane(b, &old, e, k);
                parts.push(match &m[1..4] {
                    "mla" => b.val(format!("add i{e} {acc}, {p}")),
                    "mls" => b.val(format!("sub i{e} {acc}, {p}")),
                    _ => p,
                });
            }
            let r = vbuild(b, e, &parts);
            b.write_v(d, r, 128);
        }
        "eor3" => {
            let (d, _, _) = vec_dst(&ops[0])?;
            let x = read_v128(b, &ops[1])?;
            let y = read_v128(b, &ops[2])?;
            let z = read_v128(b, &ops[3])?;
            let xy = b.val(format!("xor i128 {x}, {y}"));
            let r = b.val(format!("xor i128 {xy}, {z}"));
            b.write_v(d, r, 128);
        }
        "crc32b" | "crc32h" | "crc32w" | "crc32x" | "crc32cb" | "crc32ch" | "crc32cw" | "crc32cx" => {
            // CRC-32 (or CRC-32C) of the low bytes of the data register,
            // computed by the support code.
            let bytes = match m.as_bytes()[m.len() - 1] {
                b'b' => 1,
                b'h' => 2,
                b'w' => 4,
                _ => 8,
            };
            let (dr, _) = b.reg_op(i, 0)?;
            let (cr, cbits) = b.reg_op(i, 1)?;
            let (vr, vbits) = b.reg_op(i, 2)?;
            let crc = b.read(cr, cbits)?;
            let v = b.read(vr, vbits)?;
            let v = b.resize(v, vbits, 64, false);
            let poly: u32 = if m.starts_with("crc32c") { 0x82F6_3B78 } else { 0xEDB8_8320 };
            let r = b.val(format!("call i32 @__wasm_env_crc32(i32 {crc}, i64 {v}, i32 {bytes}, i32 {})", poly as i32));
            b.write(dr, r, 32)?;
        }
        "scvtf" | "ucvtf" if has_arrspec(&ops[0]) => {
            let op = if m == "scvtf" { "sitofp" } else { "uitofp" };
            vector_map(b, i, |b, bits, lane| {
                let ty = fp_ty(bits)?.0;
                let f = b.val(format!("{op} i{bits} {lane} to {ty}"));
                Ok(b.val(format!("bitcast {ty} {f} to i{bits}")))
            })?
        }
        "scvtf" | "ucvtf" => {
            let (d, dbits) = fp_reg(&ops[0])?;
            let (ty, _) = fp_ty(dbits)?;
            // The integer comes from a general register or, for the scalar
            // SIMD form (scvtf d0, d1), from a SIMD/FP register.
            let (x, sbits) = if is_v(ops.get(1)) {
                let (n, sbits) = fp_reg(&ops[1])?;
                (b.read_v(n, sbits), sbits)
            } else {
                let (s, sbits) = b.reg_op(i, 1)?;
                (b.read(s, sbits)?, sbits)
            };
            let op = if m == "scvtf" { "sitofp" } else { "uitofp" };
            let mut r = b.val(format!("{op} i{sbits} {x} to {ty}"));
            // Fixed-point form (scvtf d0, x0, #fbits): scale by 2^-fbits,
            // which is exact.
            if ops.len() == 3 {
                let fbits = b.imm_op(i, 2)?;
                r = b.val(format!("fmul {ty} {r}, {}", pow2(-(fbits as i64))));
            }
            write_fp(b, d, r, dbits)?;
        }

        // --- Rounding, and conversions with a rounding mode ----------------------------------
        "frintx" | "frinti" | "frintz" | "frintp" | "frintm" | "frinta" | "frintn" => {
            let mode = m.as_bytes()[5] as char;
            if is_scalar_fp(ops) {
                let (d, bits) = fp_reg(&ops[0])?;
                let x = read_fp(b, &ops[1], bits)?;
                let r = fround(b, mode, bits, &x)?;
                write_fp(b, d, r, bits)?;
            } else {
                vector_fp_unary(b, i, |b, bits, x| fround(b, mode, bits, x))?;
            }
        }
        "fcvtas" | "fcvtau" | "fcvtns" | "fcvtnu" | "fcvtms" | "fcvtmu" | "fcvtps" | "fcvtpu"
            if has_arrspec(&ops[0]) =>
        {
            // Per lane: round in the given mode, then convert with saturation.
            let mode = m.as_bytes()[4] as char;
            let op = if m.ends_with('s') { "fptosi.sat" } else { "fptoui.sat" };
            vector_map(b, i, |b, bits, lane| {
                let (ty, s) = fp_ty(bits)?;
                let x = b.val(format!("bitcast i{bits} {lane} to {ty}"));
                let r = fround(b, mode, bits, &x)?;
                Ok(b.val(format!("call i{bits} @llvm.{op}.i{bits}.{s}({ty} {r})")))
            })?
        }
        "uqadd" | "sqadd" | "uqsub" | "sqsub" if is_v(ops.first()) => {
            // Saturating add/subtract (vector or scalar). FPSR.QC isn't modelled.
            let f = match m {
                "uqadd" => "uadd.sat",
                "sqadd" => "sadd.sat",
                "uqsub" => "usub.sat",
                _ => "ssub.sat",
            };
            let (d, elem, total) = vec_dst(&ops[0])?;
            let x = read_v128(b, &ops[1])?;
            let y = read_v128(b, &ops[2])?;
            let mut parts = Vec::new();
            for k in 0..nlanes(total, elem) {
                let xk = vlane(b, &x, elem, k);
                let yk = vlane(b, &y, elem, k);
                parts.push(b.val(format!("call i{elem} @llvm.{f}.i{elem}(i{elem} {xk}, i{elem} {yk})")));
            }
            let r = vbuild(b, elem, &parts);
            b.write_v(d, r, 128);
        }
        "fcvtas" | "fcvtau" | "fcvtns" | "fcvtnu" | "fcvtms" | "fcvtmu" | "fcvtps" | "fcvtpu" if !is_v(ops.first()) => {
            let (d, dbits) = b.reg_op(i, 0)?;
            let (_, sbits) = fp_reg(&ops[1])?;
            let (sty, s) = fp_ty(sbits)?;
            let x = read_fp(b, &ops[1], sbits)?;
            let rounded = fround(b, m.as_bytes()[4] as char, sbits, &x)?;
            let op = if m.ends_with('s') { "fptosi.sat" } else { "fptoui.sat" };
            let r = b.val(format!("call i{dbits} @llvm.{op}.i{dbits}.{s}({sty} {rounded})"));
            b.write(d, r, dbits)?;
        }

        // --- SIMD floating point, per lane ---------------------------------------------------
        "fneg" | "fabs" | "fsqrt" if has_arrspec(&ops[0]) => vector_fp_unary(b, i, |b, bits, x| {
            let (ty, s) = fp_ty(bits)?;
            Ok(match m {
                "fneg" => b.val(format!("fneg {ty} {x}")),
                "fabs" => b.val(format!("call {ty} @llvm.fabs.{s}({ty} {x})")),
                _ => {
                    let r = b.val(format!("call {ty} @llvm.sqrt.{s}({ty} {x})"));
                    fp_exceptions(b, ty, &[x], &r, false);
                    r
                }
            })
        })?,
        "fadd" | "fsub" | "fmul" | "fdiv" | "fmax" | "fmin" | "fmaxnm" | "fminnm" if has_arrspec(&ops[0]) => {
            let (d, elem, total) = vec_dst(&ops[0])?;
            let ty = fp_ty(elem)?.0;
            let x = read_v128(b, &ops[1])?;
            let (y, ylane) = second_operand(b, &ops[2])?;
            let mut parts = Vec::new();
            for k in 0..nlanes(total, elem) {
                let xl = vlane(b, &x, elem, k);
                let yl = vlane(b, &y, elem, ylane.unwrap_or(k));
                let xf = b.val(format!("bitcast i{elem} {xl} to {ty}"));
                let yf = b.val(format!("bitcast i{elem} {yl} to {ty}"));
                let r = match m {
                    "fadd" | "fsub" | "fmul" | "fdiv" => {
                        let r = b.val(format!("{m} {ty} {xf}, {yf}"));
                        fp_exceptions(b, ty, &[&xf, &yf], &r, m == "fdiv");
                        r
                    }
                    _ => fp_minmax(b, m, ty, &xf, &yf),
                };
                parts.push(b.val(format!("bitcast {ty} {r} to i{elem}")));
            }
            let r = vbuild(b, elem, &parts);
            b.write_v(d, r, 128);
        }

        // --- SIMD bitwise select, table lookup, reversal, widening -------------------------
        "bsl" | "bit" | "bif" => {
            let (d, _, total) = vec_dst(&ops[0])?;
            let old = b.read_v(d, 128);
            let n = read_v128(b, &ops[1])?;
            let mm = read_v128(b, &ops[2])?;
            // result = (t AND sel) OR (f AND NOT sel)
            let (sel, t, f) = match m {
                "bsl" => (old, n, mm),
                "bit" => (mm, n, old),
                _ => (mm, old, n),
            };
            let a = b.val(format!("and i128 {t}, {sel}"));
            let ns = b.val(format!("xor i128 {sel}, -1"));
            let c = b.val(format!("and i128 {f}, {ns}"));
            let mut r = b.val(format!("or i128 {a}, {c}"));
            if total < 128 {
                r = b.val(format!("and i128 {r}, {}", konst128((1u128 << total) - 1)));
            }
            b.write_v(d, r, 128);
        }
        "tbl" | "tbx" => {
            let (d, _, total) = vec_dst(&ops[0])?;
            let (regs, _, _) = reg_list(&ops[1])?;
            // Each table register as two 64-bit halves, so the per-byte
            // selection is a native 64-bit shift.
            let mut halves = Vec::new();
            for &n in &regs {
                let v = b.read_v(n, 128);
                let lo = b.val(format!("trunc i128 {v} to i64"));
                let h = b.val(format!("lshr i128 {v}, 64"));
                let hi = b.val(format!("trunc i128 {h} to i64"));
                halves.push((lo, hi));
            }
            let idxv = read_v128(b, &ops[2])?;
            let old = if m == "tbx" { Some(b.read_v(d, 128)) } else { None };
            let mut parts = Vec::new();
            for k in 0..total / 8 {
                let idx = vlane(b, &idxv, 8, k);
                // Out-of-range indices give 0 (tbl) or keep the old byte (tbx).
                let mut acc = match &old {
                    Some(o) => vlane(b, o, 8, k),
                    None => "0".into(),
                };
                let bit3 = b.val(format!("and i8 {idx}, 8"));
                let upper = b.val(format!("icmp ne i8 {bit3}, 0"));
                let low3 = b.val(format!("and i8 {idx}, 7"));
                let low3w = b.val(format!("zext i8 {low3} to i64"));
                let sh = b.val(format!("shl i64 {low3w}, 3"));
                let which = b.val(format!("lshr i8 {idx}, 4"));
                for (j, (lo, hi)) in halves.iter().enumerate() {
                    let half = b.val(format!("select i1 {upper}, i64 {hi}, i64 {lo}"));
                    let s = b.val(format!("lshr i64 {half}, {sh}"));
                    let byte = b.val(format!("trunc i64 {s} to i8"));
                    let hit = b.val(format!("icmp eq i8 {which}, {j}"));
                    acc = b.val(format!("select i1 {hit}, i8 {byte}, i8 {acc}"));
                }
                parts.push(acc);
            }
            let r = vbuild(b, 8, &parts);
            b.write_v(d, r, 128);
        }
        "rev16" | "rev32" | "rev64" if is_v(ops.first()) => {
            // Reverse the elements within each 16/32/64-bit container.
            let (d, elem, total) = vec_dst(&ops[0])?;
            let per = m[3..].parse::<u32>()? / elem;
            let x = read_v128(b, &ops[1])?;
            let mut parts = Vec::new();
            for k in 0..nlanes(total, elem) {
                parts.push(vlane(b, &x, elem, k / per * per + (per - 1 - k % per)));
            }
            let r = vbuild(b, elem, &parts);
            b.write_v(d, r, 128);
        }
        "sxtl" | "uxtl" | "sxtl2" | "uxtl2" | "sshll" | "ushll" | "sshll2" | "ushll2" | "shll" | "shll2" => {
            // (shll shifts by the element size, pushing out any sign bits, so
            // its extension doesn't matter.)
            // Widen the low (or, for the "2" forms, high) half's lanes.
            let (d, e, _) = vec_dst(&ops[0])?;
            let x = read_v128(b, &ops[1])?;
            let n = 128 / e;
            let first = if m.ends_with('2') { n } else { 0 };
            let shift = if m.contains("shll") { b.imm_op(i, 2)? } else { 0 };
            let ext = if m.starts_with('s') { "sext" } else { "zext" };
            let mut parts = Vec::new();
            for k in 0..n {
                let lane = vlane(b, &x, e / 2, first + k);
                let w = b.val(format!("{ext} i{} {lane} to i{e}", e / 2));
                parts.push(if shift == 0 { w } else { b.val(format!("shl i{e} {w}, {shift}")) });
            }
            let r = vbuild(b, e, &parts);
            b.write_v(d, r, 128);
        }
        "fcvtzs" | "fcvtzu" if has_arrspec(&ops[0]) => {
            let op = if m == "fcvtzs" { "fptosi.sat" } else { "fptoui.sat" };
            vector_map(b, i, |b, bits, lane| {
                let (ty, s) = fp_ty(bits)?;
                let x = b.val(format!("bitcast i{bits} {lane} to {ty}"));
                fp_convert_exceptions(b, ty, &x, bits, m == "fcvtzs");
                Ok(b.val(format!("call i{bits} @llvm.{op}.i{bits}.{s}({ty} {x})")))
            })?
        }
        "fcvtzs" | "fcvtzu" => {
            let (sn, sbits) = fp_reg(&ops[1])?;
            let (sty, s) = fp_ty(sbits)?;
            let mut x = read_fp(b, &ops[1], sbits)?;
            // Fixed-point form (fcvtzs x0, d0, #fbits): scale by 2^fbits first.
            if ops.len() == 3 {
                let fbits = b.imm_op(i, 2)?;
                x = b.val(format!("fmul {sty} {x}, {}", pow2(fbits as i64)));
            }
            let op = if m == "fcvtzs" { "fptosi.sat" } else { "fptoui.sat" };
            // To a general register, or (fcvtzs d0, d1) a SIMD one.
            if is_v(ops.first()) {
                let (dn, dbits) = fp_reg(&ops[0])?;
                fp_convert_exceptions(b, sty, &x, dbits, m == "fcvtzs");
                let r = b.val(format!("call i{dbits} @llvm.{op}.i{dbits}.{s}({sty} {x})"));
                b.write_v(dn, r, dbits);
            } else {
                let (d, dbits) = b.reg_op(i, 0)?;
                fp_convert_exceptions(b, sty, &x, dbits, m == "fcvtzs");
                let r = b.val(format!("call i{dbits} @llvm.{op}.i{dbits}.{s}({sty} {x})"));
                b.write(d, r, dbits)?;
            }
            let _ = sn;
        }
        "fabd" => {
            // |x - y|, scalar or per lane.
            if is_scalar_fp(ops) {
                let (d, bits) = fp_reg(&ops[0])?;
                let (ty, s) = fp_ty(bits)?;
                let x = read_fp(b, &ops[1], bits)?;
                let y = read_fp(b, &ops[2], bits)?;
                let diff = b.val(format!("fsub {ty} {x}, {y}"));
                let r = b.val(format!("call {ty} @llvm.fabs.{s}({ty} {diff})"));
                write_fp(b, d, r, bits)?;
            } else {
                let (d, elem, total) = vec_dst(&ops[0])?;
                let (ty, s) = fp_ty(elem)?;
                let x = read_v128(b, &ops[1])?;
                let y = read_v128(b, &ops[2])?;
                let mut parts = Vec::new();
                for k in 0..nlanes(total, elem) {
                    let xl = vlane(b, &x, elem, k);
                    let yl = vlane(b, &y, elem, k);
                    let xf = b.val(format!("bitcast i{elem} {xl} to {ty}"));
                    let yf = b.val(format!("bitcast i{elem} {yl} to {ty}"));
                    let diff = b.val(format!("fsub {ty} {xf}, {yf}"));
                    let r = b.val(format!("call {ty} @llvm.fabs.{s}({ty} {diff})"));
                    parts.push(b.val(format!("bitcast {ty} {r} to i{elem}")));
                }
                let r = vbuild(b, elem, &parts);
                b.write_v(d, r, 128);
            }
        }

        // --- Atomics (ARMv8.1 LSE): WebAssembly atomic read-modify-writes
        // (LLVM expands the ones WebAssembly lacks into compare-and-swap
        // loops). ---------------------------------------------------------------------------
        _ if lse_atomic(m).is_some() => {
            let (op, width) = lse_atomic(m).unwrap();
            let (s, sbits) = b.reg_op(i, 0)?;
            let width = width.unwrap_or(sbits);
            // stADD-style aliases have no destination; cas/swp/ldOP have one.
            let (t, mem) = if ops.len() == 2 { (None, &ops[1]) } else { (Some(b.reg_op(i, 1)?), &ops[2]) };
            let addr = b.address(mem)?;
            let sv = b.read(s, sbits)?;
            let sv = b.resize(sv, sbits, width, false);
            let old = match op {
                "clr" => {
                    let ns = b.val(format!("xor i{width} {sv}, -1"));
                    b.rmw("and", &addr, &ns, width)
                }
                "cas" => {
                    // cas: store Rt if memory equals Rs; Rs receives the old value.
                    let (tr, tbits) = t.unwrap();
                    let tv = b.read(tr, tbits)?;
                    let tv = b.resize(tv, tbits, width, false);
                    b.cmpxchg(&addr, &sv, &tv, width).0
                }
                _ => {
                    let rmw = match op {
                        "eor" => "xor",
                        "set" => "or",
                        "smax" => "max",
                        "smin" => "min",
                        "swp" => "xchg",
                        other => other, // add, umax, umin
                    };
                    b.rmw(rmw, &addr, &sv, width)
                }
            };
            // The old value, zero-extended, goes to Rt (Rs for cas).
            let dest = if op == "cas" { Some((s, sbits)) } else { t };
            if let Some((dr, dbits)) = dest {
                let v = b.resize(old, width, dbits, false);
                b.write(dr, v, dbits)?;
            }
        }
        "fcvt" => {
            let (d, dbits) = fp_reg(&ops[0])?;
            let (_, sbits) = fp_reg(&ops[1])?;
            let x = read_fp(b, &ops[1], sbits)?;
            let (dty, _) = fp_ty(dbits)?;
            let (sty, _) = fp_ty(sbits)?;
            let r = if dbits > sbits {
                b.val(format!("fpext {sty} {x} to {dty}"))
            } else {
                b.val(format!("fptrunc {sty} {x} to {dty}"))
            };
            if dbits < sbits {
                // Overflow when a finite value is too large for the narrower type.
                let wide = b.val(format!("fpext {dty} {r} to {sty}"));
                fp_exceptions(b, sty, &[&x], &wide, false);
            }
            write_fp(b, d, r, dbits)?;
        }

        other => bail!("unsupported instruction `{other}`"),
    }
    Ok(false)
}

// ===========================================================================
// Helpers
// ===========================================================================

fn cond_from(s: &str) -> Result<Condition> {
    use Condition::*;
    Ok(match s {
        "eq" => EQ,
        "ne" => NE,
        "cs" | "hs" => CS,
        "cc" | "lo" => CC,
        "mi" => MI,
        "pl" => PL,
        "vs" => VS,
        "vc" => VC,
        "hi" => HI,
        "ls" => LS,
        "ge" => GE,
        "lt" => LT,
        "gt" => GT,
        "le" => LE,
        "al" => AL,
        "nv" => NV,
        _ => bail!("condition {s}"),
    })
}

/// Whether an operand is a SIMD/FP register (a V-family register, or any
/// register with a vector arrangement).
fn is_v(op: Option<&Operand>) -> bool {
    matches!(op, Some(Operand::Reg { reg: r, arrspec }) if arrspec.is_some() || matches!(reg(*r), Ok((R::V(_), _))))
}

fn has_arrspec(op: &Operand) -> bool {
    matches!(op, Operand::Reg { arrspec: Some(_), .. })
}

/// Whether an operand names a single vector element, like `v0.s[2]`.
fn is_elem(op: &Operand) -> bool {
    matches!(op, Operand::Reg { arrspec: Some(a), .. } if a.lane().is_some())
}

/// (vector index, element bits, lane) for an element operand.
fn elem_of(op: &Operand) -> Option<(u8, u32, u32)> {
    if let Operand::Reg { reg: r, arrspec: Some(a) } = op
        && let (Some(k), Ok((R::V(n), _))) = (a.lane(), reg(*r))
    {
        return Some((n, elem_bits(a), k));
    }
    None
}

/// Whether this is a scalar (not arranged) floating-point instruction:
/// the destination is an S/D/H register with no arrangement.
fn is_scalar_fp(ops: &[Operand]) -> bool {
    matches!(ops.first(), Some(Operand::Reg { reg: r, arrspec: None })
        if matches!(reg(*r), Ok((R::V(_), 16 | 32 | 64))))
}

/// (vector index, width bits) of a scalar SIMD/FP register operand.
fn fp_reg(op: &Operand) -> Result<(u8, u32)> {
    match op {
        Operand::Reg { reg: r, .. } => match reg(*r)? {
            (R::V(n), bits) => Ok((n, bits)),
            _ => bail!("expected an FP register"),
        },
        _ => bail!("expected an FP register, got {op:?}"),
    }
}

fn fp_ty(bits: u32) -> Result<(&'static str, &'static str)> {
    match bits {
        16 => Ok(("half", "f16")),
        32 => Ok(("float", "f32")),
        64 => Ok(("double", "f64")),
        _ => bail!("{bits}-bit floating point"),
    }
}

/// Reads a scalar FP operand as an LLVM float/double value.
fn read_fp(b: &mut Block, op: &Operand, bits: u32) -> Result<String> {
    let (n, _) = fp_reg(op)?;
    let ty = fp_ty(bits)?.0;
    let iv = b.read_v(n, bits);
    Ok(b.val(format!("bitcast i{bits} {iv} to {ty}")))
}

/// The address of a SIMD structure load/store (ld1-4, st1-4, ld1r). bad64
/// decodes the post-index immediate form (`[x2], #16`) as the base register
/// plus a separate immediate operand, so the writeback happens here.
fn structure_address(b: &mut Block, i: &Instruction) -> Result<String> {
    let ops = i.operands();
    let addr = b.address(&ops[1])?;
    if let (Operand::MemReg(r), Some(Operand::Imm32 { .. } | Operand::Imm64 { .. })) = (&ops[1], ops.get(2)) {
        let step = b.imm_op(i, 2)?;
        let next = b.val(format!("add i64 {addr}, {}", step as i64));
        b.write(reg(*r)?.0, next, 64)?;
    }
    Ok(addr)
}

/// The second source of a vector operation: a whole vector, or a single
/// element (`v2.s[3]`, the "by element" forms) as the vector holding it and
/// the lane to use for every lane of the result.
fn second_operand(b: &mut Block, op: &Operand) -> Result<(String, Option<u32>)> {
    Ok(match elem_of(op) {
        Some((n, _, lane)) => (b.read_v(n, 128), Some(lane)),
        None => (read_v128(b, op)?, None),
    })
}

/// Decodes an LSE atomic mnemonic: ldOP/stOP (OP one of add, clr, eor, set,
/// smax, smin, umax, umin), swp or cas, each with optional acquire/release
/// ordering (a, l, al) and size (b, h). Returns (operation, explicit width).
fn lse_atomic(m: &str) -> Option<(&'static str, Option<u32>)> {
    const OPS: [&str; 8] = ["add", "clr", "eor", "set", "smax", "smin", "umax", "umin"];
    let (op, rest) = if let Some(r) = m.strip_prefix("swp") {
        ("swp", r)
    } else if let Some(r) = m.strip_prefix("cas") {
        ("cas", r)
    } else {
        let r = m.strip_prefix("ld").or_else(|| m.strip_prefix("st"))?;
        let op = OPS.iter().find(|o| r.starts_with(**o))?;
        (*op, &r[op.len()..])
    };
    let (order, size) = match rest.strip_suffix('b') {
        Some(o) => (o, Some(8)),
        None => match rest.strip_suffix('h') {
            Some(o) => (o, Some(16)),
            None => (rest, None),
        },
    };
    matches!(order, "" | "a" | "l" | "al").then_some((op, size))
}

/// Rounds a float/double as an arm64 FRINT<mode> or FCVT<mode> does:
/// x/i the current mode (nearest, ties to even), n ties to even, z toward
/// zero, p toward +infinity, m toward -infinity, a ties away from zero.
fn fround(b: &mut Block, mode: char, bits: u32, x: &str) -> Result<String> {
    let (ty, s) = fp_ty(bits)?;
    Ok(match mode {
        'x' | 'i' => b.val(format!("call {ty} @llvm.rint.{s}({ty} {x})")),
        'n' => b.val(format!("call {ty} @llvm.roundeven.{s}({ty} {x})")),
        'z' => b.val(format!("call {ty} @llvm.trunc.{s}({ty} {x})")),
        'p' => b.val(format!("call {ty} @llvm.ceil.{s}({ty} {x})")),
        'm' => b.val(format!("call {ty} @llvm.floor.{s}({ty} {x})")),
        'a' => {
            // No WebAssembly instruction does this (and llvm.round needs
            // libm): truncate, then step away from zero if the dropped part
            // is at least 1/2. x - trunc(x) is exact, so the result is too.
            let t = b.val(format!("call {ty} @llvm.trunc.{s}({ty} {x})"));
            let f = b.val(format!("fsub {ty} {x}, {t}"));
            let af = b.val(format!("call {ty} @llvm.fabs.{s}({ty} {f})"));
            let up = b.fcmp("oge", ty, &af, "5.000000e-01");
            let one = b.val(format!("call {ty} @llvm.copysign.{s}({ty} 1.000000e+00, {ty} {x})"));
            let stepped = b.val(format!("fadd {ty} {t}, {one}"));
            b.val(format!("select i1 {up}, {ty} {stepped}, {ty} {t}"))
        }
        _ => bail!("rounding mode {mode}"),
    })
}

/// 2^e as an LLVM floating-point constant (exact in float and double for
/// the fixed-point scales arm64 allows).
fn pow2(e: i64) -> String {
    format!("0x{:016X}", ((1023 + e) as u64) << 52)
}

/// Applies `f` to each lane of a vector, as integers of the lane width.
fn vector_map(b: &mut Block, i: &Instruction, f: impl Fn(&mut Block, u32, &str) -> Result<String>) -> Result<()> {
    let (d, elem, total) = vec_dst(&i.operands()[0])?;
    let x = read_v128(b, &i.operands()[1])?;
    let mut parts = Vec::new();
    for k in 0..nlanes(total, elem) {
        let lane = vlane(b, &x, elem, k);
        parts.push(f(b, elem, &lane)?);
    }
    let r = vbuild(b, elem, &parts);
    b.write_v(d, r, 128);
    Ok(())
}

/// Applies a floating-point function to each lane of a vector.
fn vector_fp_unary(b: &mut Block, i: &Instruction, f: impl Fn(&mut Block, u32, &str) -> Result<String>) -> Result<()> {
    vector_map(b, i, |b, bits, lane| {
        let ty = fp_ty(bits)?.0;
        let x = b.val(format!("bitcast i{bits} {lane} to {ty}"));
        let r = f(b, bits, &x)?;
        Ok(b.val(format!("bitcast {ty} {r} to i{bits}")))
    })
}

fn write_fp(b: &mut Block, idx: u8, val: String, bits: u32) -> Result<()> {
    let ty = fp_ty(bits)?.0;
    let iv = b.val(format!("bitcast {ty} {val} to i{bits}"));
    b.write_v(idx, iv, bits);
    Ok(())
}

/// FPSR cumulative exception flags.
const FPSR_IOC: u32 = 1; // invalid operation

/// Raises the FPSR exceptions an arithmetic result `r` of `inputs` implies:
/// invalid, divide by zero (`div`: x / y) and overflow (see
/// Block::check_fp).
fn fp_exceptions(b: &mut Block, ty: &str, inputs: &[&str], r: &str, div: bool) {
    b.check_fp(ty, inputs, r, div);
}

/// Raises FPSR's invalid flag if converting `x` (type `ty`) to a `bits`-wide
/// integer is out of range or NaN, as FCVTZS/FCVTZU and friends do (they
/// saturate).
fn fp_convert_exceptions(b: &mut Block, ty: &str, x: &str, bits: u32, signed: bool) {
    let (lo, hi) = if signed { (-(2f64.powi(bits as i32 - 1)), 2f64.powi(bits as i32 - 1)) } else { (-1.0, 2f64.powi(bits as i32)) };
    let (lo, hi) = (format!("0x{:016X}", lo.to_bits()), format!("0x{:016X}", hi.to_bits()));
    // Widened behind a fence, or LLVM narrows these comparisons back to
    // half-precision ones, which hang its WebAssembly backend (see Block::fcmp).
    let x = if ty == "double" {
        x.to_string()
    } else {
        let w = b.val(format!("fpext {ty} {x} to double"));
        b.val(format!("call double @llvm.arithmetic.fence.f64(double {w})"))
    };
    // In range: lo < x < hi for unsigned (after truncation, x > -1), lo <= x < hi for signed.
    let above = b.val(format!("fcmp {} double {x}, {lo}", if signed { "oge" } else { "ogt" }));
    let below = b.val(format!("fcmp olt double {x}, {hi}"));
    let ok = b.val(format!("and i1 {above}, {below}"));
    let bits = b.val(format!("select i1 {ok}, i32 0, i32 {FPSR_IOC}"));
    b.raise_fp(bits);
}

/// x*y + z with a single rounding, as arm64's multiply-adds compute it.
/// WebAssembly's only fused multiply-add is relaxed SIMD's `madd`, which
/// the runtime runs deterministically (fused; see its engine config), so the
/// scalar goes through lane 0 of a vector. Half precision computes in single
/// precision.
fn fused(b: &mut Block, ty: &str, x: &str, y: &str, z: &str) -> String {
    if ty == "half" {
        let [x, y, z] = [x, y, z].map(|v| b.val(format!("fpext half {v} to float")));
        let r = fused(b, "float", &x, &y, &z);
        return b.val(format!("fptrunc float {r} to half"));
    }
    let (n, s) = if ty == "double" { (2, "v2f64") } else { (4, "v4f32") };
    let v = format!("<{n} x {ty}>");
    let [x, y, z] = [x, y, z].map(|e| b.val(format!("insertelement {v} poison, {ty} {e}, i64 0")));
    let r = b.val(format!("call {v} @llvm.wasm.relaxed.madd.{s}({v} {x}, {v} {y}, {v} {z})"));
    b.val(format!("extractelement {v} {r}, i64 0"))
}

/// arm64 FP minimum/maximum for the mnemonic `m` (fmin, fmaxnmp, fminv,
/// ...): FMIN/FMAX propagate NaN and order -0 below +0, which is
/// `llvm.minimum`/`llvm.maximum` (wasm's own f64.min/f64.max); the NM forms
/// return the other operand when exactly one is NaN.
fn fp_minmax(b: &mut Block, m: &str, ty: &str, x: &str, y: &str) -> String {
    // Scalars and whole vectors alike (`ty` may be `<4 x float>`).
    let (s, cond) = match ty {
        "double" => ("f64", "i1"),
        "float" => ("f32", "i1"),
        "half" => ("f16", "i1"),
        "<2 x double>" => ("v2f64", "<2 x i1>"),
        _ => ("v4f32", "<4 x i1>"),
    };
    let f = if m.contains("max") { "maximum" } else { "minimum" };
    let r = b.val(format!("call {ty} @llvm.{f}.{s}({ty} {x}, {ty} {y})"));
    let x_nan = b.fcmp("uno", ty, x, x);
    let y_nan = b.fcmp("uno", ty, y, y);
    if m.contains("nm") {
        // One NaN: the other operand. Both: x's NaN, as FMIN would.
        let r = b.val(format!("select {cond} {y_nan}, {ty} {x}, {ty} {r}"));
        let t = if cond == "i1" { "true" } else { "splat (i1 true)" };
        let not_y = b.val(format!("xor {cond} {y_nan}, {t}"));
        let only_x = b.val(format!("and {cond} {x_nan}, {not_y}"));
        b.val(format!("select {cond} {only_x}, {ty} {y}, {ty} {r}"))
    } else {
        // wasm's min/max give a canonical NaN; arm64 gives the NaN operand,
        // x's first.
        let r = b.val(format!("select {cond} {y_nan}, {ty} {y}, {ty} {r}"));
        b.val(format!("select {cond} {x_nan}, {ty} {x}, {ty} {r}"))
    }
}

/// Sets N Z C V from comparing FP values x and y (arm64 FPCompare).
fn fp_compare(b: &mut Block, ty: &str, x: &str, y: &str) {
    let lt = b.fcmp("olt", ty, x, y);
    let eq = b.fcmp("oeq", ty, x, y);
    // C is set unless strictly less-than and ordered; V is set when unordered.
    let c = b.fcmp("uge", ty, x, y);
    let v = b.fcmp("uno", ty, x, y);
    b.set_flags(lt, eq, c, v);
    let _ = (N, Z, C, V);
}

/// System register name, lowercased.
fn sysreg(op: &Operand) -> Result<String> {
    match op {
        Operand::SysReg(r) => Ok(format!("{r}").to_lowercase()),
        other => bail!("expected a system register, got {other:?}"),
    }
}

/// (memory access bits, sign-extend) for a load/store mnemonic.
fn access_width(m: &str, reg_bits: u32) -> (u32, bool) {
    if m.contains("sb") {
        (8, true)
    } else if m.contains("sh") {
        (16, true)
    } else if m.ends_with("sw") {
        (32, true)
    } else if m.ends_with('b') || m.contains("rb") {
        (8, false)
    } else if m.ends_with('h') || m.contains("rh") {
        (16, false)
    } else {
        (reg_bits, false)
    }
}

/// (vector index, element bits, total bits) of a vector destination.
fn vec_dst(op: &Operand) -> Result<(u8, u32, u32)> {
    match op {
        Operand::Reg { reg: r, arrspec } => {
            let (rr, bits) = reg(*r)?;
            let R::V(n) = rr else { bail!("not a vector register") };
            match arrspec {
                Some(a) => Ok((n, elem_bits(a), arr_bits(a))),
                None => Ok((n, bits, bits)),
            }
        }
        _ => bail!("expected a vector register, got {op:?}"),
    }
}

/// Reads a whole 128-bit vector operand.
fn read_v128(b: &mut Block, op: &Operand) -> Result<String> {
    let (n, _, _) = vec_dst(op)?;
    Ok(b.read_v(n, 128))
}

/// A multiplier that replicates a zero-extended element across every lane.
fn broadcast(elem: u32, total: u32) -> u128 {
    let mut m = 0u128;
    let mut k = 0;
    while k * elem < total {
        m |= 1u128 << (k * elem);
        k += 1;
    }
    m
}

/// Extracts lane `k` (of `elem` bits) from a 128-bit vector value.
/// A 128-bit register value as a vector of `elem`-bit floats (`<4 x float>`,
/// `<2 x double>`), for whole-vector arithmetic. For a 64-bit arrangement
/// (`.2s`, or one element) the upper half is zeroed first: those lanes then
/// compute zeros, raising no FP exceptions and leaving the result's upper
/// half zero, as arm64 requires.
fn fp_vec_in(b: &mut Block, v: &str, elem: u32, total: u32) -> Result<(String, String)> {
    let ty = fp_ty(elem)?.0;
    let vt = format!("<{} x {ty}>", 128 / elem);
    let v = if total < 128 {
        b.val(format!("and i128 {v}, {}", konst128((1u128 << total) - 1)))
    } else {
        v.to_string()
    };
    Ok((b.val(format!("bitcast i128 {v} to {vt}")), vt))
}

/// The second source of a vector FP operation as a vector (see fp_vec_in):
/// a whole register, or one element (`v2.s[3]`) in every active lane.
fn fp_second_vec(b: &mut Block, op: &Operand, elem: u32, total: u32) -> Result<String> {
    let (v, lane) = second_operand(b, op)?;
    let Some(lane) = lane else { return Ok(fp_vec_in(b, &v, elem, total)?.0) };
    let (full, vt) = fp_vec_in(b, &v, elem, 128)?;
    let n = 128 / elem;
    // Lanes past the arrangement take element n, the first of the zeros.
    let mask: Vec<String> =
        (0..n).map(|k| format!("i32 {}", if k * elem < total { lane } else { n })).collect();
    Ok(b.val(format!("shufflevector {vt} {full}, {vt} zeroinitializer, <{n} x i32> <{}>", mask.join(", "))))
}

/// Through LLVM vector types (`extractelement` of `<N x iE>`), not 128-bit
/// shifts: wasm has no i128, and lane-wise code built this way is what
/// LLVM's SLP vectorizer turns back into SIMD.
fn vlane(b: &mut Block, v: &str, elem: u32, k: u32) -> String {
    let n = 128 / elem;
    let vec = b.val(format!("bitcast i128 {v} to <{n} x i{elem}>"));
    b.val(format!("extractelement <{n} x i{elem}> {vec}, i32 {k}"))
}

/// Combines lanes (each `elem` bits) into a 128-bit vector value; lanes
/// beyond `parts` are zero.
fn vbuild(b: &mut Block, elem: u32, parts: &[String]) -> String {
    let n = 128 / elem;
    let mut acc = "zeroinitializer".to_string();
    for (k, p) in parts.iter().enumerate() {
        acc = b.val(format!("insertelement <{n} x i{elem}> {acc}, i{elem} {p}, i32 {k}"));
    }
    b.val(format!("bitcast <{n} x i{elem}> {acc} to i128"))
}

/// A register list operand (`{v0.16b}`): (indices, element bits, total bits).
fn reg_list(op: &Operand) -> Result<(Vec<u8>, u32, u32)> {
    match op {
        Operand::Reg { .. } => {
            let (n, e, t) = vec_dst(op)?;
            Ok((vec![n], e, t))
        }
        Operand::MultiReg { regs, arrspec } => {
            let mut indices = Vec::new();
            for r in regs.iter().flatten() {
                if let (R::V(n), _) = reg(*r)? {
                    indices.push(n);
                }
            }
            let a = arrspec.as_ref().ok_or_else(|| anyhow::anyhow!("register list without arrangement"))?;
            Ok((indices, elem_bits(a), arr_bits(a)))
        }
        _ => bail!("expected a register list, got {op:?}"),
    }
}

/// The lane of a single-lane register list (`{v0.s, v1.s}[1]`), if any.
fn list_lane(op: &Operand) -> Option<u32> {
    match op {
        Operand::Reg { arrspec: Some(a), .. } | Operand::MultiReg { arrspec: Some(a), .. } => a.lane(),
        _ => None,
    }
}

fn nlanes(total: u32, elem: u32) -> u32 {
    total / elem
}

/// Element-wise vector arithmetic, comparison and min/max.
fn vector_binary(b: &mut Block, i: &Instruction, m: &str) -> Result<()> {
    let (d, elem, total) = vec_dst(&i.operands()[0])?;
    let x = read_v128(b, &i.operands()[1])?;
    // The second operand is a register, or an immediate (cmeq v0.4s, v1.4s, #0).
    // The second operand is a register, an element (mul v0.4s, v1.4s,
    // v2.s[1]), or an immediate (cmeq v0.4s, v1.4s, #0).
    let y = match &i.operands()[2] {
        Operand::Imm32 { .. } | Operand::Imm64 { .. } => Err(konst(b.imm_op(i, 2)?, elem)),
        op => Ok(second_operand(b, op)?),
    };
    let mut parts = Vec::new();
    for k in 0..nlanes(total, elem) {
        let xk = vlane(b, &x, elem, k);
        let yk = match &y {
            Ok((y, lane)) => vlane(b, y, elem, lane.unwrap_or(k)),
            Err(imm) => imm.clone(),
        };
        let r = match m {
            "add" => b.val(format!("add i{elem} {xk}, {yk}")),
            "sub" => b.val(format!("sub i{elem} {xk}, {yk}")),
            "mul" => b.val(format!("mul i{elem} {xk}, {yk}")),
            "and" => b.val(format!("and i{elem} {xk}, {yk}")),
            "orr" => b.val(format!("or i{elem} {xk}, {yk}")),
            "eor" => b.val(format!("xor i{elem} {xk}, {yk}")),
            "umin" => b.val(format!("call i{elem} @llvm.umin.i{elem}(i{elem} {xk}, i{elem} {yk})")),
            "umax" => b.val(format!("call i{elem} @llvm.umax.i{elem}(i{elem} {xk}, i{elem} {yk})")),
            "smin" => b.val(format!("call i{elem} @llvm.smin.i{elem}(i{elem} {xk}, i{elem} {yk})")),
            "smax" => b.val(format!("call i{elem} @llvm.smax.i{elem}(i{elem} {xk}, i{elem} {yk})")),
            "uabd" | "sabd" => {
                let p = if m == "uabd" { "ugt" } else { "sgt" };
                let gt = b.val(format!("icmp {p} i{elem} {xk}, {yk}"));
                let hi = b.val(format!("sub i{elem} {xk}, {yk}"));
                let lo = b.val(format!("sub i{elem} {yk}, {xk}"));
                b.val(format!("select i1 {gt}, i{elem} {hi}, i{elem} {lo}"))
            }
            // Comparisons: all-ones on true, all-zeros on false.
            _ => {
                let pred = match m {
                    "cmeq" => "eq",
                    "cmgt" => "sgt",
                    "cmge" => "sge",
                    "cmle" => "sle",
                    "cmlt" => "slt",
                    "cmhi" => "ugt",
                    "cmhs" => "uge",
                    "cmtst" => "ne", // (x & y) != 0
                    _ => bail!("vector op {m}"),
                };
                let cmp = if m == "cmtst" {
                    let a = b.val(format!("and i{elem} {xk}, {yk}"));
                    b.val(format!("icmp ne i{elem} {a}, 0"))
                } else {
                    b.val(format!("icmp {pred} i{elem} {xk}, {yk}"))
                };
                b.val(format!("sext i1 {cmp} to i{elem}"))
            }
        };
        parts.push(r);
    }
    // vbuild returns a full i128 with unused high lanes zeroed (the arm64
    // rule: writing a 64-bit vector result clears the upper half).
    let r = vbuild(b, elem, &parts);
    b.write_v(d, r, 128);
    Ok(())
}

/// Element-wise vector unary operations.
fn vector_unary(b: &mut Block, i: &Instruction, m: &str) -> Result<()> {
    let (d, elem, total) = vec_dst(&i.operands()[0])?;
    let x = read_v128(b, &i.operands()[1])?;
    let mut parts = Vec::new();
    for k in 0..nlanes(total, elem) {
        let xk = vlane(b, &x, elem, k);
        let r = match m {
            "neg" => b.val(format!("sub i{elem} 0, {xk}")),
            "not" | "mvn" => b.val(format!("xor i{elem} {xk}, -1")),
            "abs" => {
                let neg = b.val(format!("sub i{elem} 0, {xk}"));
                let isneg = b.val(format!("icmp slt i{elem} {xk}, 0"));
                b.val(format!("select i1 {isneg}, i{elem} {neg}, i{elem} {xk}"))
            }
            "cnt" => b.val(format!("call i{elem} @llvm.ctpop.i{elem}(i{elem} {xk})")),
            "rbit" => b.val(format!("call i{elem} @llvm.bitreverse.i{elem}(i{elem} {xk})")),
            _ => bail!("vector unary {m}"),
        };
        parts.push(r);
    }
    // vbuild returns a full i128 with unused high lanes zeroed (the arm64
    // rule: writing a 64-bit vector result clears the upper half).
    let r = vbuild(b, elem, &parts);
    b.write_v(d, r, 128);
    Ok(())
}

/// Vector shift by an immediate or by a register (per lane).
fn vector_shift(b: &mut Block, i: &Instruction, m: &str) -> Result<()> {
    let (d, elem, total) = vec_dst(&i.operands()[0])?;
    let x = read_v128(b, &i.operands()[1])?;
    let mut parts = Vec::new();
    for k in 0..nlanes(total, elem) {
        let xk = vlane(b, &x, elem, k);
        let r = match m {
            "shl" => {
                let amt = b.imm_op(i, 2)?;
                b.val(format!("shl i{elem} {xk}, {}", konst(amt, elem)))
            }
            "sshr" => {
                let amt = b.imm_op(i, 2)?;
                b.val(format!("ashr i{elem} {xk}, {}", konst(amt, elem)))
            }
            "ushr" => {
                let amt = b.imm_op(i, 2)?;
                b.val(format!("lshr i{elem} {xk}, {}", konst(amt, elem)))
            }
            // Register shift: the low byte of each lane is a signed count;
            // negative counts shift right (logically for ushl, arithmetically
            // for sshl), and counts of the element width or more give 0 (or,
            // shifting sshl right, the sign in every bit).
            "sshl" | "ushl" => {
                let y = read_v128(b, &i.operands()[2])?;
                let yk = vlane(b, &y, elem, k);
                let byte = if elem == 8 { yk } else { b.val(format!("trunc i{elem} {yk} to i8")) };
                let s = b.val(format!("sext i8 {byte} to i32"));
                let neg = b.val(format!("icmp slt i32 {s}, 0"));
                let minus = b.val(format!("sub i32 0, {s}"));
                let mag = b.val(format!("select i1 {neg}, i32 {minus}, i32 {s}"));
                let big = b.val(format!("icmp uge i32 {mag}, {elem}"));
                let clamped = b.val(format!("select i1 {big}, i32 {}, i32 {mag}", elem - 1));
                let amount = match elem {
                    32 => clamped,
                    64 => b.val(format!("zext i32 {clamped} to i64")),
                    _ => b.val(format!("trunc i32 {clamped} to i{elem}")),
                };
                let left = b.val(format!("shl i{elem} {xk}, {amount}"));
                let left = b.val(format!("select i1 {big}, i{elem} 0, i{elem} {left}"));
                let right = if m == "ushl" {
                    let r = b.val(format!("lshr i{elem} {xk}, {amount}"));
                    b.val(format!("select i1 {big}, i{elem} 0, i{elem} {r}"))
                } else {
                    b.val(format!("ashr i{elem} {xk}, {amount}")) // clamped: sign fill
                };
                b.val(format!("select i1 {neg}, i{elem} {right}, i{elem} {left}"))
            }
            _ => bail!("vector shift {m}"),
        };
        parts.push(r);
    }
    // vbuild returns a full i128 with unused high lanes zeroed (the arm64
    // rule: writing a 64-bit vector result clears the upper half).
    let r = vbuild(b, elem, &parts);
    b.write_v(d, r, 128);
    Ok(())
}

/// Horizontal reductions across the lanes of a vector.
fn vector_reduce(b: &mut Block, i: &Instruction, m: &str) -> Result<()> {
    let (d, _) = fp_reg(&i.operands()[0]).map(|(n, _)| (n, ()))?;
    let (_, elem, total) = vec_dst(&i.operands()[1])?;
    let x = read_v128(b, &i.operands()[1])?;
    let widen = matches!(m, "uaddlv" | "saddlv");
    let acc_bits = if widen { elem * 2 } else { elem };
    let lane0 = vlane(b, &x, elem, 0);
    let mut acc = if widen {
        let e = if m == "uaddlv" { "zext" } else { "sext" };
        b.val(format!("{e} i{elem} {lane0} to i{acc_bits}"))
    } else {
        lane0
    };
    for k in 1..nlanes(total, elem) {
        let lane = vlane(b, &x, elem, k);
        let lane = if widen {
            let e = if m == "uaddlv" { "zext" } else { "sext" };
            b.val(format!("{e} i{elem} {lane} to i{acc_bits}"))
        } else {
            lane
        };
        acc = match m {
            "addv" | "uaddlv" | "saddlv" => b.val(format!("add i{acc_bits} {acc}, {lane}")),
            "umaxv" => b.val(format!("call i{acc_bits} @llvm.umax.i{acc_bits}(i{acc_bits} {acc}, i{acc_bits} {lane})")),
            "uminv" => b.val(format!("call i{acc_bits} @llvm.umin.i{acc_bits}(i{acc_bits} {acc}, i{acc_bits} {lane})")),
            "smaxv" => b.val(format!("call i{acc_bits} @llvm.smax.i{acc_bits}(i{acc_bits} {acc}, i{acc_bits} {lane})")),
            "sminv" => b.val(format!("call i{acc_bits} @llvm.smin.i{acc_bits}(i{acc_bits} {acc}, i{acc_bits} {lane})")),
            _ => bail!("reduction {m}"),
        };
    }
    b.write_v(d, acc, acc_bits);
    Ok(())
}

/// Permutations: uzp1/uzp2 (deinterleave), zip1/zip2 (interleave),
/// trn1/trn2 (transpose).
fn vector_permute(b: &mut Block, i: &Instruction, m: &str) -> Result<()> {
    let (d, elem, total) = vec_dst(&i.operands()[0])?;
    let x = read_v128(b, &i.operands()[1])?;
    let y = read_v128(b, &i.operands()[2])?;
    let lanes = nlanes(total, elem);
    let mut parts = Vec::new();
    for k in 0..lanes {
        let (src, lane) = match m {
            "uzp1" | "uzp2" => {
                let base = if m == "uzp1" { 0 } else { 1 };
                let idx = base + 2 * k;
                (idx >= lanes, idx % lanes)
            }
            "zip1" | "zip2" => {
                let half = if m == "zip1" { 0 } else { lanes / 2 };
                let pair = half + k / 2;
                (k % 2 == 1, pair)
            }
            "trn1" | "trn2" => {
                let off = if m == "trn1" { 0 } else { 1 };
                (k % 2 == 1, (k & !1) + off)
            }
            _ => bail!("permute {m}"),
        };
        let v = if src { &y } else { &x };
        parts.push(vlane(b, v, elem, lane));
    }
    // vbuild returns a full i128 with unused high lanes zeroed (the arm64
    // rule: writing a 64-bit vector result clears the upper half).
    let r = vbuild(b, elem, &parts);
    b.write_v(d, r, 128);
    Ok(())
}

/// Narrowing: shrn (shift right and narrow) and xtn (extract narrow). The
/// `2` variants write the high 64 bits; the base forms write the low half.
fn vector_narrow(b: &mut Block, i: &Instruction, m: &str) -> Result<()> {
    let high = m.ends_with('2');
    let (d, out_elem, _) = vec_dst(&i.operands()[0])?;
    let (_, in_elem, in_total) = vec_dst(&i.operands()[1])?;
    let x = read_v128(b, &i.operands()[1])?;
    let amt = if m.starts_with("shrn") { b.imm_op(i, 2)? } else { 0 };
    let lanes = nlanes(in_total, in_elem);
    let mut parts = Vec::new();
    for k in 0..lanes {
        let lane = vlane(b, &x, in_elem, k);
        let shifted = if amt == 0 { lane } else { b.val(format!("lshr i{in_elem} {lane}, {amt}")) };
        parts.push(b.val(format!("trunc i{in_elem} {shifted} to i{out_elem}")));
    }
    let narrowed = vbuild(b, out_elem, &parts);
    if high {
        // Keep the existing low 64 bits, place the result in the high half.
        let old = b.read_v(d, 128);
        let low = b.val(format!("and i128 {old}, 18446744073709551615"));
        let hi = b.val(format!("shl i128 {narrowed}, 64"));
        let r = b.val(format!("or i128 {low}, {hi}"));
        b.write_v(d, r, 128);
    } else {
        b.write_v(d, narrowed, 128);
    }
    Ok(())
}

/// fmov in its several forms: gpr<->fp bit moves, reg-reg copy, and moving
/// a general register to or from a vector element.
fn fmov(b: &mut Block, i: &Instruction) -> Result<()> {
    let ops = i.operands();
    let d = &ops[0];
    let s = &ops[1];
    // fmov to/from a vector element: fmov x, v.d[1] / fmov v.d[1], x.
    if let Some((n, elem, k)) = elem_of(d) {
        let (sr, sbits) = b.reg_op(i, 1)?;
        let val = b.read(sr, sbits)?;
        let val = b.resize(val, sbits, elem, false);
        insert_lane(b, n, elem, k, val);
        return Ok(());
    }
    if let Some((n, elem, k)) = elem_of(s) {
        let (dr, dbits) = b.reg_op(i, 0)?;
        let v = b.read_v(n, 128);
        let lane = vlane(b, &v, elem, k);
        let v = b.resize(lane, elem, dbits, false);
        b.write(dr, v, dbits)?;
        return Ok(());
    }
    match (reg(reg_of(d)?)?, reg_of(s).and_then(reg).ok()) {
        // fmov <Sd|Dd>, <Wn|Xn>: bits from a general register.
        ((R::V(n), bits), Some((R::X(_) | R::Zr, _))) => {
            let v = b.operand(s, bits)?;
            b.write_v(n, v, bits);
        }
        // fmov <Wd|Xd>, <Sn|Dn>: bits to a general register.
        ((R::X(_) | R::Zr, bits), Some((R::V(sn), _))) => {
            let (d, _) = b.reg_op(i, 0)?;
            let v = b.read_v(sn, bits);
            b.write(d, v, bits)?;
        }
        // fmov <Sd|Dd>, <Sn|Dn>: register copy.
        ((R::V(n), bits), Some((R::V(sn), _))) => {
            let v = b.read_v(sn, bits);
            b.write_v(n, v, bits);
        }
        // fmov <Vd>.<T>, #imm: the immediate in every lane.
        ((R::V(n), _), None) if has_arrspec(d) => {
            let (_, elem, total) = vec_dst(d)?;
            let imm = fp_immediate(s, elem)?;
            let r = konst128(u128::from(imm) * broadcast(elem, total));
            b.write_v(n, r, 128);
        }
        // fmov <Sd|Dd>, #imm: floating-point immediate.
        ((R::V(n), bits), None) => {
            let imm = fp_immediate(s, bits)?;
            b.write_v(n, konst(imm, bits), bits);
        }
        _ => bail!("unsupported fmov form"),
    }
    Ok(())
}

/// Inserts `val` (elem bits) into lane `k` of vector register `n`.
fn insert_lane(b: &mut Block, n: u8, elem: u32, k: u32, val: String) {
    let old = b.read_v(n, 128);
    let mask = if elem >= 128 { u128::MAX } else { ((1u128 << elem) - 1) << (k * elem) };
    let cleared = b.val(format!("and i128 {old}, {}", konst128(!mask)));
    let wide = b.val(format!("zext i{elem} {val} to i128"));
    let placed = if k == 0 { wide } else { b.val(format!("shl i128 {wide}, {}", k * elem)) };
    let r = b.val(format!("or i128 {cleared}, {placed}"));
    b.write_v(n, r, 128);
}

fn reg_of(op: &Operand) -> Result<Reg> {
    match op {
        Operand::Reg { reg: r, .. } => Ok(*r),
        _ => bail!("expected a register, got {op:?}"),
    }
}

/// The raw bit pattern of an fmov floating-point immediate.
fn fp_immediate(op: &Operand, bits: u32) -> Result<u64> {
    let f = match op {
        Operand::FImm32(v) => f32::from_bits(*v) as f64,
        _ => bail!("expected a floating-point immediate, got {op:?}"),
    };
    Ok(if bits == 32 { (f as f32).to_bits() as u64 } else { f.to_bits() })
}

/// mov/ins/umov/smov between register files and vector elements.
fn vector_move(b: &mut Block, i: &Instruction) -> Result<()> {
    let m = i.op().mnem();
    let ops = i.operands();
    let d = &ops[0];
    let s = &ops[1];

    // Whole-register copy: mov vd.T, vn.T.
    if has_arrspec(d) && has_arrspec(s) && !is_elem(d) && !is_elem(s) {
        // A 64-bit (.8b) move copies the low half and zeroes the rest.
        let (dn, _, total) = vec_dst(d)?;
        let (sn, _, _) = vec_dst(s)?;
        let sv = b.read_v(sn, total);
        b.write_v(dn, sv, total);
        return Ok(());
    }
    // mov Sd, Vn.S[k] (dup, scalar): element to a scalar SIMD register.
    if let (Some((sn, elem, k)), Ok((R::V(dn), _)), false) = (elem_of(s), reg(reg_of(d)?), has_arrspec(d)) {
        let v = b.read_v(sn, 128);
        let lane = vlane(b, &v, elem, k);
        b.write_v(dn, lane, elem);
        return Ok(());
    }
    // umov/smov/mov Wd, Vn.T[k]: element to general register.
    if let Some((sn, elem, k)) = elem_of(s) {
        if let (R::X(_) | R::Zr, dbits) = reg(reg_of(d)?)? {
            let (dr, _) = b.reg_op(i, 0)?;
            let v = b.read_v(sn, 128);
            let lane = vlane(b, &v, elem, k);
            let v = b.resize(lane, elem, dbits, m == "smov");
            b.write(dr, v, dbits)?;
            return Ok(());
        }
        // ins/mov Vd.T[j], Vn.T[k]: element to element.
        if let Some((dn, delem, dk)) = elem_of(d) {
            let sv = b.read_v(sn, 128);
            let lane = vlane(b, &sv, elem, k);
            insert_lane(b, dn, delem, dk, lane);
            return Ok(());
        }
    }
    // ins/mov Vd.T[k], Wn: general register to element.
    if let Some((dn, elem, k)) = elem_of(d) {
        let (sr, sbits) = b.reg_op(i, 1)?;
        let v = b.read(sr, sbits)?;
        let v = b.resize(v, sbits, elem, false);
        insert_lane(b, dn, elem, k, v);
        return Ok(());
    }
    bail!("unsupported {m} between {d:?} and {s:?}")
}
