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
    let in_code = |a: u64| a.is_multiple_of(4) && insns.contains_key(&a);

    let mut roots = BTreeSet::new();
    roots.insert(image.entry);
    roots.extend(image.functions.iter().map(|f| f.0));
    roots.extend(&image.roots);

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
        for (k, w) in seg.data.as_chunks::<8>().0.iter().enumerate() {
            if (seg.vaddr + k as u64 * 8).is_multiple_of(8) {
                roots.insert(u64::from_le_bytes(*w));
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
        let mut block: Vec<(u64, &Instruction)> = Vec::new();
        while let Some(i) = insns.get(&pc) {
            block.push((pc, i));
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
                // A `br` with no writeback is usually a switch: read the
                // jump table (case targets aren't stored as pointers).
                if i.op().mnem() == "br" {
                    for t in jump_table(image, &block) {
                        add(t);
                    }
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

/// A canonical general-register id (x and w of the same number share one),
/// with sp=31, fp=29, lr=30.
fn gpr_id(op: &Operand) -> Option<u32> {
    let Operand::Reg { reg: r, .. } = op else { return None };
    let n = r.name();
    match n {
        "sp" | "wsp" => Some(31),
        "fp" => Some(29),
        "lr" => Some(30),
        "xzr" | "wzr" => None,
        _ => n.strip_prefix('x').or_else(|| n.strip_prefix('w')).and_then(|s| s.parse().ok()),
    }
}

/// Recognizes the GCC/LLVM arm64 switch idiom ending in `br` and returns
/// the case target addresses by reading the offset table:
///
///     adrp xT, PAGE ; add xT, xT, #OFF     ; table base
///     ldr{b,h,sw} wI, [xT, wIdx, ext #s]   ; entry = table[index]
///     adr  xB, LABEL                        ; base label
///     add  xD, xB, wI, ext #2               ; target = LABEL + entry*4
///     br   xD
///
/// Register absolute values (from adr/adrp/add-imm/mov) are tracked across
/// the block. Table entries are read until one points outside the code.
fn jump_table(image: &Image, block: &[(u64, &Instruction)]) -> Vec<u64> {
    use bad64::{Operand::*, Shift};
    let br = block.last().unwrap().1;
    let Some(target_reg) = br.operands().first().and_then(gpr_id) else { return vec![] };

    // Track absolute register values built by adr/adrp/add-imm/mov.
    let mut val: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    // The last load into each register: (resolved table base, width, signed).
    let mut load: std::collections::HashMap<u32, (u64, u32, bool)> = std::collections::HashMap::new();
    let mut table: Option<(u64, u64, u32, bool)> = None; // (label, table_base, width, signed)

    for (_, i) in block {
        let ops = i.operands();
        let d = ops.first().and_then(gpr_id);
        match i.op().mnem() {
            "adr" | "adrp" => {
                if let (Some(d), Some(Label(imm))) = (d, ops.get(1)) {
                    val.insert(d, imm_u64(imm));
                }
            }
            "add" if matches!(ops.get(2), Some(Imm64 { .. } | Imm32 { .. })) => {
                if let (Some(d), Some(s), Some(off)) = (d, ops.get(1).and_then(gpr_id), ops.get(2)) {
                    match (val.get(&s).copied(), off) {
                        (Some(base), Imm64 { imm, .. } | Imm32 { imm, .. }) => {
                            val.insert(d, base.wrapping_add(imm_u64(imm)));
                        }
                        _ => {
                            val.remove(&d);
                        }
                    }
                }
            }
            // add xD, xB, wI, <ext> #2  -> the switch target computation.
            "add" if d == Some(target_reg) => {
                if let (Some(base_reg), Some(ShiftReg { reg: idx, shift })) =
                    (ops.get(1).and_then(gpr_id), ops.get(2))
                {
                    let scaled = matches!(shift, Shift::SXTH(2) | Shift::SXTB(2) | Shift::SXTW(2) | Shift::UXTH(2) | Shift::UXTW(2) | Shift::UXTB(2) | Shift::LSL(2));
                    if let (Some(&label), Some(idx), true) = (val.get(&base_reg), gpr_id(&Reg { reg: *idx, arrspec: None }), scaled)
                        && let Some(&(base, width, signed)) = load.get(&idx)
                    {
                        table = Some((label, base, width, signed));
                    }
                }
            }
            "ldrb" | "ldrsb" | "ldrh" | "ldrsh" | "ldrsw" => {
                let tab = match ops.get(1) {
                    Some(MemExt { regs, .. }) => gpr_id(&Reg { reg: regs[0], arrspec: None }),
                    Some(MemOffset { reg: r, .. }) => gpr_id(&Reg { reg: *r, arrspec: None }),
                    _ => None,
                };
                if let (Some(d), Some(base)) = (d, tab.and_then(|t| val.get(&t).copied())) {
                    let width = if i.op().mnem().contains('h') { 2 } else if i.op().mnem().ends_with('w') { 4 } else { 1 };
                    let signed = i.op().mnem().contains('s');
                    load.insert(d, (base, width, signed));
                }
                if let Some(d) = d {
                    val.remove(&d);
                }
            }
            "movz" | "mov" | "movn" | "movk" => {
                if let (Some(d), Some(Imm64 { imm, .. } | Imm32 { imm, .. })) = (d, ops.get(1)) {
                    val.insert(d, imm_u64(imm));
                } else if let Some(d) = d {
                    val.remove(&d);
                }
            }
            _ => {
                if let Some(d) = d {
                    val.remove(&d);
                    load.remove(&d);
                }
            }
        }
    }

    let Some((label, base, width, signed)) = table else { return vec![] };
    let mut targets = Vec::new();
    for i in 0..4096u64 {
        let Some(raw) = image.read_uint(base + i * width as u64, width) else { break };
        let entry = if signed {
            let bits = width * 8;
            (raw << (64 - bits)) as i64 >> (64 - bits)
        } else {
            raw as i64
        };
        let target = label.wrapping_add((entry * 4) as u64);
        if !image.is_code(target) {
            break;
        }
        targets.push(target);
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn program(bytes: &[u8]) -> (Image, Program) {
        let image = Image::load(bytes).unwrap();
        let prog = discover(&image);
        (image, prog)
    }

    fn symbol(image: &Image, name: &str) -> u64 {
        image.functions.iter().find(|f| f.1 == name).unwrap_or_else(|| panic!("no symbol {name}")).0
    }

    #[test]
    fn leaders_are_decoded_instructions() {
        let Some(bytes) = testutil::static_c("c/hello.c") else { return };
        let (image, prog) = program(&bytes);
        assert!(prog.leaders.contains(&image.entry));
        for a in &prog.leaders {
            assert!(prog.insns.contains_key(a), "leader {a:#x} isn't an instruction");
        }
    }

    #[test]
    fn control_flow_edges_start_blocks() {
        let Some(bytes) = testutil::static_c("c/hello.c") else { return };
        let (_, prog) = program(&bytes);
        // Every control instruction inside a reached block contributes its
        // direct target and (unless it never falls through) the next address.
        let mut checked = 0;
        for &start in &prog.leaders {
            let mut pc = start;
            while let Some(i) = prog.insns.get(&pc) {
                if is_control(i) {
                    if let Some(t) = direct_target(i).filter(|t| prog.insns.contains_key(t)) {
                        assert!(prog.leaders.contains(&t), "branch at {pc:#x} to {t:#x}");
                    }
                    if !is_terminal(i) && prog.insns.contains_key(&(pc + 4)) {
                        assert!(prog.leaders.contains(&(pc + 4)), "fall-through after {pc:#x}");
                    }
                    checked += 1;
                    break;
                }
                pc += 4;
                if prog.leaders.contains(&pc) {
                    break;
                }
            }
        }
        assert!(checked > 100, "checked {checked} blocks");
    }

    /// Addresses of instructions inside reached blocks.
    fn reached(prog: &Program) -> BTreeSet<u64> {
        let mut out = BTreeSet::new();
        for &start in &prog.leaders {
            let mut pc = start;
            while let Some(i) = prog.insns.get(&pc) {
                out.insert(pc);
                pc += 4;
                if is_control(i) || prog.leaders.contains(&pc) {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn calls_have_return_address_blocks() {
        let Some(bytes) = testutil::min() else { return };
        let (_, prog) = program(&bytes);
        let reached = reached(&prog);
        let calls: Vec<u64> = reached.iter().copied().filter(|a| prog.insns[a].op().mnem() == "bl").collect();
        assert!(!calls.is_empty());
        for pc in calls {
            assert!(prog.leaders.contains(&(pc + 4)), "return address after bl at {pc:#x}");
        }
    }

    #[test]
    fn data_in_the_code_segment_is_not_reached() {
        // min.c's strings sit after _start in the executable segment, and
        // some of those bytes decode as instructions (even calls).
        use object::{Object, ObjectSection};
        let Some(bytes) = testutil::min() else { return };
        let (image, prog) = program(&bytes);
        let file = object::File::parse(&*bytes).unwrap();
        let rodata = file.section_by_name(".rodata").expect(".rodata");
        let data = rodata.address()..rodata.address() + rodata.size();
        let exec = image.segments.iter().find(|s| s.exec).unwrap();
        assert!(exec.contains(data.start), "the fixture's strings share the executable segment");
        // _start builds the string's address with ADRP+ADD, which looks like a
        // code pointer; section information keeps it out.
        for a in reached(&prog) {
            assert!(!data.contains(&a), "data at {a:#x} was treated as code");
        }
    }

    #[test]
    fn finds_functions_reached_only_through_pointers() {
        let Some(bytes) = testutil::static_c("arm64/fnptr.c") else { return };
        let mut image = Image::load(&bytes).unwrap();
        let wanted: Vec<u64> =
            ["via_table_a", "via_table_b", "via_adrp"].iter().map(|n| symbol(&image, n)).collect();
        // Forget the symbols, as in a stripped binary: only pointers remain.
        image.functions.clear();
        let prog = discover(&image);
        for (a, name) in wanted.iter().zip(["via_table_a", "via_table_b", "via_adrp"]) {
            assert!(prog.leaders.contains(a), "{name} at {a:#x} not found");
        }
    }

    #[test]
    fn stripped_binaries_still_reach_main() {
        let Some(bytes) = testutil::static_c("c/hello.c") else { return };
        let mut image = Image::load(&bytes).unwrap();
        let main = symbol(&image, "main");
        image.functions.clear();
        let prog = discover(&image);
        assert!(prog.leaders.contains(&main), "main is passed to __libc_start_main by address");
    }

    #[test]
    fn unreachable_words_are_not_blocks() {
        let Some(bytes) = testutil::static_c("c/hello.c") else { return };
        let (_, prog) = program(&bytes);
        assert!(prog.leaders.len() < prog.insns.len() / 2, "blocks are a minority of instructions");
        // Words after an unconditional branch that nothing targets stay out.
        for (&pc, i) in &prog.insns {
            if is_terminal(i) && !prog.leaders.contains(&(pc + 4)) {
                return;
            }
        }
        panic!("expected some unreached fall-through after a terminal instruction");
    }
}
