//! Finding basic blocks in an arm64 executable.
//!
//! arm64 instructions are fixed-width and aligned, so every instruction can
//! be decoded by decoding every word. The hard part is knowing where
//! control can arrive. Roots are:
//! - the entry point and every function symbol,
//! - every address that looks like a code pointer: computed by ADR or
//!   ADRP+ADD in code, or stored as an aligned 64-bit word in data.
//!
//! From the roots, a worklist follows direct branch targets and
//! fall-through / return addresses. Everything reached is a block leader.
//! Words that decode but are never reached (literal pools, read-only data
//! sharing the executable segment) are left alone.
//!
//! Indirect branches (BR, BLR, RET) go through a dispatch table of all
//! leaders. A target that isn't a leader stops the program with an error
//! naming the address; a runtime translation fallback comes later.

use std::collections::{BTreeMap, BTreeSet};

use bad64::{Instruction, Operand};

use crate::elf::Image;

pub struct Program {
    /// Every decoded instruction by address.
    pub insns: BTreeMap<u64, Instruction>,
    /// Block start addresses reachable from the roots.
    pub leaders: BTreeSet<u64>,
}

pub fn imm_u64(imm: &bad64::Imm) -> u64 {
    match *imm {
        bad64::Imm::Signed(v) => v as u64,
        bad64::Imm::Unsigned(v) => v,
    }
}

/// Branch and call targets written as a label operand.
pub fn direct_target(i: &Instruction) -> Option<u64> {
    if !is_control(i) {
        return None;
    }
    i.operands().iter().find_map(|o| match o {
        Operand::Label(imm) => Some(imm_u64(imm)),
        _ => None,
    })
}

/// Instructions that end a block.
pub fn is_control(i: &Instruction) -> bool {
    let m = i.op().mnem();
    matches!(
        m,
        "b" | "bl" | "br" | "blr" | "ret" | "cbz" | "cbnz" | "tbz" | "tbnz" | "braa" | "brab" | "blraa"
            | "blrab" | "retaa" | "retab" | "brk" | "udf" | "hlt"
    ) || m.starts_with("b.")
}

/// Control instructions after which execution never continues at the next
/// instruction.
pub fn is_terminal(i: &Instruction) -> bool {
    matches!(i.op().mnem(), "b" | "br" | "ret" | "braa" | "brab" | "retaa" | "retab" | "brk" | "udf" | "hlt")
}

pub fn discover(image: &Image) -> Program {
    let mut insns = BTreeMap::new();
    for (addr, _, insn) in image.decode_all() {
        if let Some(i) = insn {
            insns.insert(addr, i);
        }
    }
    let in_code = |a: u64| a % 4 == 0 && insns.contains_key(&a);

    let mut roots = BTreeSet::new();
    roots.insert(image.entry);
    roots.extend(image.functions.iter().map(|f| f.0));

    // Code addresses built in registers: ADR, or ADRP followed by ADD.
    let mut prev_adrp: BTreeMap<u32, u64> = BTreeMap::new();
    for i in insns.values() {
        match (i.op().mnem(), i.operands()) {
            ("adr", [_, Operand::Label(imm)]) => {
                roots.insert(imm_u64(imm));
            }
            ("adrp", [Operand::Reg { reg, .. }, Operand::Label(imm)]) => {
                prev_adrp.insert(*reg as u32, imm_u64(imm));
            }
            ("add", [_, Operand::Reg { reg: src, .. }, Operand::Imm64 { imm, .. } | Operand::Imm32 { imm, .. }]) => {
                if let Some(page) = prev_adrp.get(&(*src as u32)) {
                    roots.insert(page.wrapping_add(imm_u64(imm)));
                }
            }
            _ => {}
        }
    }

    // Code pointers stored in data (function pointer tables, init arrays).
    for seg in &image.segments {
        for (k, w) in seg.data.chunks_exact(8).enumerate() {
            if (seg.vaddr + k as u64 * 8) % 8 == 0 {
                roots.insert(u64::from_le_bytes(w.try_into().unwrap()));
            }
        }
    }
    roots.retain(|&a| in_code(a));

    // Follow control flow from the roots.
    let mut leaders = roots.clone();
    let mut work: Vec<u64> = roots.into_iter().collect();
    let mut walked = BTreeSet::new();
    while let Some(start) = work.pop() {
        if !walked.insert(start) {
            continue;
        }
        let mut pc = start;
        while let Some(i) = insns.get(&pc) {
            if is_control(i) {
                let mut add = |a: u64| {
                    if in_code(a) && leaders.insert(a) {
                        work.push(a);
                    }
                };
                if let Some(t) = direct_target(i) {
                    add(t);
                }
                if !is_terminal(i) {
                    add(pc + 4);
                }
                break;
            }
            pc += 4;
            if leaders.contains(&pc) {
                // Runs into a block that's already (or will be) walked.
                if !walked.contains(&pc) {
                    work.push(pc);
                }
                break;
            }
        }
    }
    Program { insns, leaders }
}
