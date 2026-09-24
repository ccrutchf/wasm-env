//! Semantics of individual arm64 instructions, following the Arm
//! Architecture Reference Manual's pseudocode. `lift` returns Ok(true) when
//! the instruction ended the block (control flow); data-processing
//! instructions return Ok(false). An unsupported instruction returns Err,
//! which the caller turns into a trap that names it.

use anyhow::{Result, bail};
use bad64::{Condition, Instruction, Operand, Reg, Shift};

use super::{Block, C, N, R, V, Z, arr_bits, elem_bits, konst, konst128, reg};
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
        "svc" => b.call_support("__wasm_env_svc"),
        "nop" | "hint" | "yield" | "wfe" | "wfi" | "sev" | "sevl" | "csdb" | "isb" | "dmb" | "dsb" | "clrex"
        | "bti" | "prfm" | "prfum" | "paciasp" | "autiasp" | "pacibsp" | "autibsp" | "xpaclri" | "pacia"
| "autia" | "pacib" | "autib" | "dc" | "ic" | "dvp" | "cfp" | "cpp" => {}

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
                "tpidr_el0" => b.val("load i64, ptr @TP"),
                // Cache line 64 bytes; DC ZVA prohibited (bit 4 set), so
                // musl's memset takes its plain-store path.
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

        // --- Logic --------------------------------------------------------------
        "and" | "orr" | "eor" | "bic" | "orn" | "eon" | "ands" | "bics" | "tst" | "mvn" if is_v(ops.first()) => {
            // Whole-register bitwise operations on vectors.
            let (d, _) = b.reg_op(i, 0)?;
            let R::V(vd) = d else { unreachable!() };
            let x = read_v128(b, &ops[1])?;
            let y = if m == "mvn" { x.clone() } else { read_v128(b, &ops[2])? };
            let y = if matches!(m, "bic" | "orn" | "mvn") { b.val(format!("xor i128 {y}, -1")) } else { y };
            let op = match m {
                "and" | "bic" => "and",
                "orr" | "orn" | "mvn" => "or",
                "eor" | "eon" => "xor",
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
        | "ldrsb" | "ldursb" | "ldrsh" | "ldursh" | "ldrsw" | "ldursw" | "ldar" | "ldarb" | "ldarh" | "ldaxr"
        | "ldaxrb" | "ldaxrh" | "ldxr" | "ldxrb" | "ldxrh" | "stlr" | "stlrb" | "stlrh" | "ldapr" | "ldaprb"
        | "ldaprh" | "ldlar" | "ldlarb" | "ldlarh" | "stllr" | "stllrb" | "stllrh" | "ldtr" | "sttr" => {
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
        // Vector load/store of a single register (`ld1 {v0.16b}, [x0]`).
        "ld1" | "st1" | "ld1r" => {
            let list = &ops[0];
            let (regs, elem, total) = reg_list(list)?;
            let addr = b.address(&ops[1])?;
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
        "add" | "sub" | "mul" if is_v(ops.first()) && has_arrspec(&ops[0]) => vector_binary(b, i, m)?,
        "cmeq" | "cmgt" | "cmge" | "cmhi" | "cmhs" | "cmtst" | "umin" | "umax" | "smin" | "smax" | "uabd" | "sabd"
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
            let hi = if index == 0 {
                "0".to_string()
            } else {
                b.val(format!("shl i128 {y}, {}", 128 - index))
            };
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
                // Compare + select (no wasm fmin/fmax, and llvm.minnum needs
                // libm). Differs from arm64 only on NaN and signed zero.
                "fmin" | "fminnm" => {
                    let c = b.val(format!("fcmp olt {ty} {x}, {y}"));
                    b.val(format!("select i1 {c}, {ty} {x}, {ty} {y}"))
                }
                _ => {
                    let c = b.val(format!("fcmp ogt {ty} {x}, {y}"));
                    b.val(format!("select i1 {c}, {ty} {x}, {ty} {y}"))
                }
            };
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
            write_fp(b, d, r, bits)?;
        }
        "fmadd" | "fmsub" | "fnmadd" | "fnmsub" => {
            // WebAssembly has no fused multiply-add, and lowering @llvm.fma
            // needs a libm `fma`. Use a separate multiply and add: two
            // roundings instead of one, a last-bit difference from true FMA.
            let (d, bits) = fp_reg(&ops[0])?;
            let ty = fp_ty(bits)?.0;
            let x = read_fp(b, &ops[1], bits)?;
            let y = read_fp(b, &ops[2], bits)?;
            let a = read_fp(b, &ops[3], bits)?;
            let p = b.val(format!("fmul {ty} {x}, {y}"));
            let r = match m {
                "fmadd" => b.val(format!("fadd {ty} {a}, {p}")),
                "fmsub" => b.val(format!("fsub {ty} {a}, {p}")),
                "fnmsub" => b.val(format!("fsub {ty} {p}, {a}")),
                _ => {
                    let s = b.val(format!("fadd {ty} {a}, {p}"));
                    b.val(format!("fneg {ty} {s}")) // fnmadd = -(a + x*y)
                }
            };
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
                _ => b.val(format!("call {ty} @llvm.sqrt.{s}({ty} {x})")),
            })
        })?,
        "fadd" | "fsub" | "fmul" | "fdiv" | "fmax" | "fmin" | "fmaxnm" | "fminnm" if has_arrspec(&ops[0]) => {
            let (d, elem, total) = vec_dst(&ops[0])?;
            let ty = fp_ty(elem)?.0;
            let x = read_v128(b, &ops[1])?;
            let y = read_v128(b, &ops[2])?;
            let mut parts = Vec::new();
            for k in 0..nlanes(total, elem) {
                let xl = vlane(b, &x, elem, k);
                let yl = vlane(b, &y, elem, k);
                let xf = b.val(format!("bitcast i{elem} {xl} to {ty}"));
                let yf = b.val(format!("bitcast i{elem} {yl} to {ty}"));
                let r = match m {
                    "fadd" | "fsub" | "fmul" | "fdiv" => b.val(format!("{m} {ty} {xf}, {yf}")),
                    _ => {
                        let pred = if m.starts_with("fmax") { "ogt" } else { "olt" };
                        let c = b.val(format!("fcmp {pred} {ty} {xf}, {yf}"));
                        b.val(format!("select i1 {c}, {ty} {xf}, {ty} {yf}"))
                    }
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
        "sxtl" | "uxtl" | "sxtl2" | "uxtl2" | "sshll" | "ushll" | "sshll2" | "ushll2" => {
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
        "fcvtzs" | "fcvtzu" => {
            let (d, dbits) = b.reg_op(i, 0)?;
            let (sn, sbits) = fp_reg(&ops[1])?;
            let (_, s) = fp_ty(sbits)?;
            let x = read_fp(b, &ops[1], sbits)?;
            let op = if m == "fcvtzs" { "fptosi.sat" } else { "fptoui.sat" };
            let r = b.val(format!("call i{dbits} @llvm.{op}.i{dbits}.{s}({} {x})", fp_ty(sbits)?.0));
            let _ = sn;
            b.write(d, r, dbits)?;
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
            let up = b.val(format!("fcmp oge {ty} {af}, 5.000000e-01"));
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

/// Sets N Z C V from comparing FP values x and y (arm64 FPCompare).
fn fp_compare(b: &mut Block, ty: &str, x: &str, y: &str) {
    let lt = b.val(format!("fcmp olt {ty} {x}, {y}"));
    let eq = b.val(format!("fcmp oeq {ty} {x}, {y}"));
    // C is set unless strictly less-than and ordered; V is set when unordered.
    let c = b.val(format!("fcmp uge {ty} {x}, {y}"));
    let v = b.val(format!("fcmp uno {ty} {x}, {y}"));
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
fn vlane(b: &mut Block, v: &str, elem: u32, k: u32) -> String {
    let shifted = if k == 0 { v.to_string() } else { b.val(format!("lshr i128 {v}, {}", k * elem)) };
    b.val(format!("trunc i128 {shifted} to i{elem}"))
}

/// Combines lanes (each `elem` bits) into a 128-bit vector value.
fn vbuild(b: &mut Block, elem: u32, parts: &[String]) -> String {
    let mut acc = "0".to_string();
    for (k, p) in parts.iter().enumerate() {
        let z = b.val(format!("zext i{elem} {p} to i128"));
        let s = if k == 0 { z } else { b.val(format!("shl i128 {z}, {}", k as u32 * elem)) };
        acc = if k == 0 { s } else { b.val(format!("or i128 {acc}, {s}")) };
    }
    acc
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

fn nlanes(total: u32, elem: u32) -> u32 {
    total / elem
}

/// Element-wise vector arithmetic, comparison and min/max.
fn vector_binary(b: &mut Block, i: &Instruction, m: &str) -> Result<()> {
    let (d, elem, total) = vec_dst(&i.operands()[0])?;
    let x = read_v128(b, &i.operands()[1])?;
    let y = read_v128(b, &i.operands()[2])?;
    let mut parts = Vec::new();
    for k in 0..nlanes(total, elem) {
        let xk = vlane(b, &x, elem, k);
        let yk = vlane(b, &y, elem, k);
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
            // Register shift: the low byte of each lane is a signed count.
            "sshl" | "ushl" => {
                let y = read_v128(b, &i.operands()[2])?;
                let yk = vlane(b, &y, elem, k);
                let shift = b.val(format!("and i{elem} {yk}, {}", elem - 1));
                if m == "ushl" {
                    b.val(format!("shl i{elem} {xk}, {shift}"))
                } else {
                    b.val(format!("ashr i{elem} {xk}, {shift}"))
                }
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
        let (dn, _, total) = vec_dst(d)?;
        let sv = read_v128(b, s)?;
        b.write_v(dn, sv, total);
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
