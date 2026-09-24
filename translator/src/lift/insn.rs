//! Semantics of individual arm64 instructions, following the Arm
//! Architecture Reference Manual's pseudocode. `lift` returns Ok(true) when
//! the instruction ended the block.

use anyhow::{Result, bail};
use bad64::{Condition, Instruction, Operand, Shift};

use super::{Block, C, R, arr_bits, elem_bits, imm_i64, konst, konst128, reg};
use crate::discover::{direct_target, imm_u64};

pub fn lift(b: &mut Block, i: &Instruction) -> Result<bool> {
    let pc = i.address();
    let m = i.op().mnem();
    let ops = i.operands();
    if let Some(c) = m.strip_prefix("b.") {
        let cond = b.cond(cond_from(c)?);
        b.branch(cond, direct_target(i).unwrap(), pc + 4);
        return Ok(true);
    }
    match m {
        // --- Control flow -----------------------------------------------------
        "b" => b.jump(direct_target(i).unwrap()),
        "bl" => {
            b.write(R::X(30), konst(pc + 4, 64), 64)?;
            b.jump(direct_target(i).unwrap());
        }
        "br" | "blr" => {
            let (r, _) = b.reg_op(i, 0)?;
            let target = b.read(r, 64)?;
            if m == "blr" {
                b.write(R::X(30), konst(pc + 4, 64), 64)?;
            }
            b.jump_to(target);
        }
        "ret" => {
            let r = if ops.is_empty() { R::X(30) } else { b.reg_op(i, 0)?.0 };
            let target = b.read(r, 64)?;
            b.jump_to(target);
        }
        "cbz" | "cbnz" => {
            let (r, bits) = b.reg_op(i, 0)?;
            let v = b.read(r, bits)?;
            let cmp = if m == "cbz" { "eq" } else { "ne" };
            let cond = b.val(format!("icmp {cmp} i{bits} {v}, 0"));
            b.branch(cond, direct_target(i).unwrap(), pc + 4);
        }
        "tbz" | "tbnz" => {
            let (r, bits) = b.reg_op(i, 0)?;
            let bit = b.imm_op(i, 1)?;
            let v = b.read(r, bits)?;
            let shifted = b.val(format!("lshr i{bits} {v}, {bit}"));
            let low = b.val(format!("trunc i{bits} {shifted} to i1"));
            let cond = if m == "tbz" { b.not(low) } else { low };
            b.branch(cond, direct_target(i).unwrap(), pc + 4);
        }
        "svc" => {
            b.call_support("__wasm_env_svc");
            return Ok(false);
        }
        "brk" | "udf" | "hlt" => b.fail(pc, &format!("`{i}` executed")),
        "nop" | "hint" | "yield" | "wfe" | "wfi" | "sev" | "sevl" | "csdb" | "isb" | "dmb" | "dsb" | "clrex"
        | "bti" | "prfm" | "prfum" | "paciasp" | "autiasp" | "pacibsp" | "autibsp" | "xpaclri" => return Ok(false),

        // --- Moves and address computation --------------------------------------
        "mov" | "movz" | "orr" if is_v(ops.first()) || is_v(ops.get(1)) || ops.iter().any(is_elem) => {
            vector_move(b, i)?
        }
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
                "tpidr_el0" => b.val("load i64, ptr @TP"),
                // Cache line 64 bytes; DC ZVA prohibited (bit 4), so memset
                // takes its plain-store path.
                "ctr_el0" => konst(0x8444_c004, 64),
                "dczid_el0" => konst(0x10, 64),
                "midr_el1" | "fpcr" | "fpsr" | "cntfrq_el0" | "cntvct_el0" => konst(0, 64),
                other => bail!("system register {other}"),
            };
            b.write(d, v, 64)?;
        }
        "msr" => {
            let name = sysreg(&ops[0])?;
            let v = b.operand(&ops[1], 64)?;
            match name.as_str() {
                "tpidr_el0" => b.line(format!("store i64 {v}, ptr @TP")),
                "fpcr" | "fpsr" => {}
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

        // --- Logic ------------------------------------------------------------------
        "and" | "orr" | "eor" | "bic" | "orn" | "eon" | "ands" | "bics" | "tst" | "mvn" if is_v(ops.first()) => {
            // Whole-register bitwise operations on vectors.
            let (d, _) = b.reg_op(i, 0)?;
            let R::V(vd) = d else { unreachable!() };
            let x = b.read_any(b.reg_op(i, 1)?)?;
            let y = b.read_any(b.reg_op(i, 2)?)?;
            let x = b.resize(x, b.reg_op(i, 1)?.1, 128, false);
            let y = b.resize(y, b.reg_op(i, 2)?.1, 128, false);
            let y = if matches!(m, "bic" | "orn") { b.val(format!("xor i128 {y}, -1")) } else { y };
            let op = match m {
                "and" | "bic" => "and",
                "orr" | "orn" => "or",
                "eor" => "xor",
                _ => bail!("vector {m}"),
            };
            let r = b.val(format!("{op} i128 {x}, {y}"));
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
        "lsl" | "lsr" | "asr" | "ror" | "lslv" | "lsrv" | "asrv" | "rorv" => {
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
                    // bfxil: insert a field extracted from src into the low bits.
                    let old = b.read(d, bits)?;
                    let kept = b.val(format!("and i{bits} {old}, {}", konst(!mask, bits)));
                    let shifted = b.val(format!("lshr i{bits} {src}, {lsb}"));
                    let field = b.val(format!("and i{bits} {shifted}, {}", konst(mask, bits)));
                    b.val(format!("or i{bits} {kept}, {field}"))
                }
            };
            b.write(d, r, bits)?;
        }
        "sxtb" | "sxth" | "sxtw" | "uxtb" | "uxth" => {
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
                    // Swap the bytes within each 16-bit half.
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
                    // d = cond ? f(s) : s, i.e. csinc/csinv/csneg with the
                    // condition inverted and both sources the same.
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
                _ => b.val(format!("add i{bits} {y}, 1")), // cset: 0 or 1
            };
            // For cset/csetm the "true" value is the alternative.
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
        | "ldrsb" | "ldursb" | "ldrsh" | "ldursh" | "ldrsw" | "ldursw" | "ldar" | "ldarb" | "ldarh" | "ldaxr"
        | "ldaxrb" | "ldaxrh" | "ldxr" | "ldxrb" | "ldxrh" | "stlr" | "stlrb" | "stlrh" | "ldapr" | "ldaprb"
        | "ldaprh" | "ldlar" | "ldlarb" | "ldlarh" | "stllr" | "stllrb" | "stllrh" | "ldtr" | "sttr" => {
            let r = b.reg_op(i, 0)?;
            let (mem_bits, signed) = access_width(m, r.1);
            let addr = b.address(&ops[1])?;
            if m.starts_with("st") {
                let v = b.read_any(r)?;
                let v = b.resize(v, r.1, mem_bits, false);
                b.store(&addr, &v, mem_bits);
            } else {
                let v = b.load(&addr, mem_bits);
                let v = if let R::V(_) = r.0 { v } else { b.resize(v, mem_bits, r.1, signed) };
                b.write_any((r.0, if let R::V(_) = r.0 { mem_bits } else { r.1 }), v)?;
            }
        }
        "stxr" | "stlxr" | "stxrb" | "stlxrb" | "stxrh" | "stlxrh" => {
            // Single-threaded: the store always succeeds (status 0).
            let status = b.reg_op(i, 0)?;
            let r = b.reg_op(i, 1)?;
            let (mem_bits, _) = access_width(m, r.1);
            let addr = b.address(&ops[2])?;
            let v = b.read(r.0, r.1)?;
            let v = b.resize(v, r.1, mem_bits, false);
            b.store(&addr, &v, mem_bits);
            b.write(status.0, "0".into(), status.1)?;
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
                } else {
                    let v = b.load(&a, mem_bits);
                    let v = if let R::V(_) = r.0 { v } else { b.resize(v, mem_bits, r.1, signed) };
                    b.write_any(r, v)?;
                }
            }
        }

        // --- SIMD/FP moves and immediates ------------------------------------------------------
        "fmov" => fmov(b, i)?,
        "dup" | "movi" | "mvni" if is_v(ops.first()) => {
            let (d, e, total) = vec_dst(&ops[0])?;
            let elem = match m {
                "dup" => {
                    let s = b.reg_op(i, 1)?;
                    let v = b.read_any(s)?;
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
        "ins" | "umov" | "smov" => vector_move(b, i)?,

        // --- Scalar floating point ---------------------------------------------------------
        "fadd" | "fsub" | "fmul" | "fdiv" | "fmin" | "fmax" | "fminnm" | "fmaxnm" | "fnmul" if is_scalar_fp(ops) => {
            let (d, bits) = b.reg_op(i,