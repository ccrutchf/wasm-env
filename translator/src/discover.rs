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

/// The image's instructions, decoded when asked for. Keeping every decoded
/// instruction would cost hundreds of bytes each, too much for a library
/// with tens of millions of them (libtorch), and decoding is cheap.
#[derive(Clone, Copy)]
pub struct Insns<'a> {
    image: &'a Image,
}

impl<'a> Insns<'a> {
    pub fn new(image: &'a Image) -> Self {
        Insns { image }
    }

    /// The instruction at `addr`, if it's in code and decodes.
    pub fn get(&self, addr: &u64) -> Option<Instruction> {
        if !self.image.is_code(*addr) {
            return None;
        }
        let word = self.image.read_uint(*addr, 4)? as u32;
        crate::elf::decode(word, *addr)
    }

    pub fn contains_key(&self, addr: &u64) -> bool {
        self.get(addr).is_some()
    }

    /// The decoded instructions in `range` (of code addresses), in order.
    pub fn range(&self, range: std::ops::Range<u64>) -> impl DoubleEndedIterator<Item = (u64, Instruction)> + '_ {
        let start = range.start.next_multiple_of(4);
        (0..range.end.saturating_sub(start).div_ceil(4)).filter_map(move |k| {
            let a = start + 4 * k;
            self.get(&a).map(|i| (a, i))
        })
    }

    /// Every decoded instruction, in address order.
    pub fn iter(&self) -> impl Iterator<Item = (u64, Instruction)> + '_ {
        self.image.code.iter().flat_map(move |r| self.range(r.clone()))
    }
}

pub struct Program<'a> {
    /// Every decoded instruction by address.
    pub insns: Insns<'a>,
    /// Block start addresses reachable from the roots.
    pub leaders: BTreeSet<u64>,
    /// Blocks that may be entered other than by a direct branch or falling
    /// through: the roots, jump table targets, and where calls and system
    /// calls return to (the lifter adds targets of branches from other
    /// regions). The rest need no entry into their region (see lift).
    pub entries: BTreeSet<u64>,
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

/// Instructions that end a block. SVC does too: a vfork parent resumes at
/// the instruction after its clone (see __wasm_env_svc_at in support/runtime.c),
/// so that must be a block.
pub fn is_control(i: &Instruction) -> bool {
    let m = i.op().mnem();
    matches!(
        m,
        "b" | "bl" | "br" | "blr" | "ret" | "cbz" | "cbnz" | "tbz" | "tbnz" | "braa" | "brab" | "blraa"
            | "blrab" | "retaa" | "retab" | "brk" | "udf" | "hlt" | "svc"
    ) || m.starts_with("b.")
}

/// Control instructions after which execution never continues at the next
/// instruction.
pub fn is_terminal(i: &Instruction) -> bool {
    matches!(i.op().mnem(), "b" | "br" | "ret" | "braa" | "brab" | "retaa" | "retab" | "brk" | "udf" | "hlt")
}

pub fn discover(image: &Image) -> Program<'_> {
    let insns = Insns::new(image);
    let in_code = |a: u64| insns.contains_key(&a);

    let mut roots = BTreeSet::new();
    // (Generated code has no entry of its own: its base is just where the
    // mapping starts.)
    if !image.generated {
        roots.insert(image.entry);
    }
    roots.extend(image.functions.iter().map(|f| f.0));
    roots.extend(&image.roots);
    // Where a run jumped to code the translation missed, the code around it
    // is reached in ways discovery can't follow (an interpreter's handlers,
    // entered through a table it builds at run time, say), so there the
    // instruction after each one that doesn't fall through starts a block
    // too: one missed handler reveals its neighbours. (Data read this way
    // becomes blocks that fail only if reached.)
    const SWEEP: u64 = 0x1000;
    let swept: BTreeSet<u64> = image.missed.iter().map(|&a| a / SWEEP * SWEEP).collect();
    for &r in &swept {
        for (a, i) in insns.range(r..r + SWEEP) {
            if is_terminal(&i) && in_code(a + 4) {
                roots.insert(a + 4);
            }
        }
    }

    // One pass over the code (each decodes every instruction again):
    // - Branch Target Identification landing pads (bti; paciasp/pacibsp at
    //   function entries act as one): in binaries built with branch
    //   protection, as Ubuntu's are, every legal indirect-branch target has
    //   one, so they name the targets of jump tables and computed gotos;
    // - code addresses built in registers: ADR, or ADRP followed by ADD;
    // - every direct branch, indexed by its target (see Code).
    let mut prev_adrp: BTreeMap<u32, u64> = BTreeMap::new();
    // (Not over generated code: a JIT's buffer is entered where the program
    // says, and scanning all of it for each new piece would cost more than
    // translating the piece.)
    let mut incoming: std::collections::HashMap<u64, Vec<u64>> = std::collections::HashMap::new();
    for (a, i) in insns.iter().take(if image.generated { 0 } else { usize::MAX }) {
        if let Some(t) = direct_target(&i) {
            incoming.entry(t).or_default().push(a);
        }
        match (i.op().mnem(), i.operands()) {
            ("bti" | "paciasp" | "pacibsp", _) => {
                roots.insert(a);
            }
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
    for seg in image.segments.iter().filter(|_| !image.generated) {
        for (k, w) in seg.data.as_chunks::<8>().0.iter().enumerate() {
            if (seg.vaddr + k as u64 * 8).is_multiple_of(8) {
                roots.insert(u64::from_le_bytes(*w));
            }
        }
    }
    roots.retain(|&a| in_code(a));

    // Generated code is followed only within the pages its roots are on: a
    // JIT packs code objects densely, and following branches into their
    // neighbours would translate them all again with each new one (and
    // grow each module over more pages, see the runtime's translate_code).
    // Branches out go through dispatch, translated when first taken.
    let root_pages: BTreeSet<u64> = roots.iter().map(|&a| a / SWEEP).collect();
    let followed = |a: u64| !image.generated || root_pages.contains(&(a / SWEEP));
    let code = Code { insns, incoming };
    // Follow control flow from the roots.
    let mut leaders = roots.clone();
    let mut entries = roots.clone();
    let mut work: Vec<u64> = roots.into_iter().collect();
    let mut walked = BTreeSet::new();
    while let Some(start) = work.pop() {
        if !walked.insert(start) {
            continue;
        }
        let mut pc = start;
        while let Some(i) = insns.get(&pc) {
            let i = &i;
            if is_control(i) {
                let mut add = |a: u64| {
                    if in_code(a) && followed(a) && leaders.insert(a) {
                        work.push(a);
                    }
                };
                if let Some(t) = direct_target(i) {
                    add(t);
                }
                if !is_terminal(i) {
                    add(pc + 4);
                    if matches!(i.op().mnem(), "bl" | "blr" | "blraa" | "blrab" | "svc") && in_code(pc + 4) {
                        entries.insert(pc + 4);
                    }
                }
                // A `br` with no writeback is usually a switch: read the
                // jump table (case targets aren't stored as pointers).
                if i.op().mnem() == "br" {
                    // The idiom is straight-line code ending at the br, but other
                    // leaders may split it into blocks, so look at the preceding
                    // instructions directly, back to one that doesn't fall through.
                    let mut window = vec![(pc, i.clone())];
                    while window.len() < 32 {
                        let Some(prev) = window[0].0.checked_sub(4).and_then(|a| insns.get(&a).map(|p| (a, p))) else { break };
                        if is_terminal(&prev.1) {
                            break;
                        }
                        window.insert(0, prev);
                    }
                    let targets = jump_table(image, &code, &window);
                    // WASM_ENV_DEBUG_BR=ADDR (hex) reports what was found for that br.
                    if std::env::var("WASM_ENV_DEBUG_BR").ok().and_then(|a| u64::from_str_radix(a.trim_start_matches("0x"), 16).ok()) == Some(pc) {
                        eprintln!("br at {pc:#x}: window from {:#x} ({} insns), {} targets: {:x?}", window[0].0, window.len(), targets.len(), targets);
                    }
                    for t in targets {
                        add(t);
                        if in_code(t) {
                            entries.insert(t);
                        }
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
    Program { insns, leaders, entries }
}

/// The decoded code, with every direct branch indexed by its target, for
/// tracing values back through the paths that reach an instruction.
struct Code<'a> {
    insns: Insns<'a>,
    incoming: std::collections::HashMap<u64, Vec<u64>>,
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

/// A recognized jump table.
struct Table {
    /// The address entries are relative to.
    label: u64,
    /// The table itself, its entry width in bytes, and whether entries load
    /// sign-extended.
    base: u64,
    width: u32,
    signed: bool,
    /// The dispatching add's extension (how an entry is interpreted).
    ext: bad64::Shift,
    /// The index register (for finding the bounds check).
    index: Option<u32>,
    /// Bytes per entry unit: 4 for instruction counts, 1 for byte offsets.
    scale: i64,
}

/// Recognizes the GCC/LLVM arm64 switch idiom ending in `br` and returns
/// the case target addresses by reading the offset table:
///
/// ```text
/// adrp xT, PAGE ; add xT, xT, #OFF     ; table base
/// ldr{b,h,sw} wI, [xT, wIdx, ext #s]   ; entry = table[index]
/// adr  xB, LABEL                        ; base label
/// add  xD, xB, wI, ext #2               ; target = LABEL + entry*4
/// br   xD
/// ```
///
/// Register absolute values (from adr/adrp/add-imm/mov) are tracked across
/// the block. Table entries are read until one points outside the code.
fn jump_table(image: &Image, code: &Code, block: &[(u64, Instruction)]) -> Vec<u64> {
    let insns = code.insns;
    use bad64::{Operand::*, Shift};
    let br = &block.last().unwrap().1;
    let Some(target_reg) = br.operands().first().and_then(gpr_id) else { return vec![] };

    // Track absolute register values built by adr/adrp/add-imm/mov.
    let mut val: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    // The last load into each register: (resolved table base, width, signed,
    // index register).
    let mut load: std::collections::HashMap<u32, (u64, u32, bool, Option<u32>)> = std::collections::HashMap::new();
    let mut table: Option<Table> = None;

    for (addr, i) in block.iter().map(|(a, i)| (*a, i)) {
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
                    let label = val.get(&base_reg).copied().or_else(|| register_value_before(code, addr, base_reg, 0));
                    if let (Some(label), Some(idx), true) = (label, gpr_id(&Reg { reg: *idx, arrspec: None }), scaled)
                        && let Some(&(base, width, signed, index)) = load.get(&idx)
                    {
                        table = Some(Table { label, base, width, signed, ext: *shift, index, scale: 4 });
                    }
                }
                // add xD, xB, xR: GCC's computed goto (labels as values), where
                // xR is a 32-bit byte offset from a table (glibc's printf).
                if let (Some(base_reg), Some(r @ Reg { .. })) = (ops.get(1).and_then(gpr_id), ops.get(2))
                    && let Some(idx) = gpr_id(r)
                    && let Some(&(base, 4, signed, index)) = load.get(&idx)
                    && let Some(label) = val.get(&base_reg).copied().or_else(|| register_value_before(code, addr, base_reg, 0))
                {
                    table = Some(Table { label, base, width: 4, signed, ext: Shift::LSL(0), index, scale: 1 });
                }
            }
            "ldrb" | "ldrsb" | "ldrh" | "ldrsh" | "ldrsw" => {
                let (tab, index) = match ops.get(1) {
                    Some(MemExt { regs, .. }) => {
                        (gpr_id(&Reg { reg: regs[0], arrspec: None }), gpr_id(&Reg { reg: regs[1], arrspec: None }))
                    }
                    Some(MemOffset { reg: r, .. }) => (gpr_id(&Reg { reg: *r, arrspec: None }), None),
                    _ => (None, None),
                };
                // The table base, from this window or, if it was set up earlier
                // (before a loop, say), the nearest preceding definition.
                let base = tab.and_then(|t| val.get(&t).copied().or_else(|| register_value_before(code, addr, t, 0)));
                if let (Some(d), Some(base)) = (d, base) {
                    let width = if i.op().mnem().contains('h') { 2 } else if i.op().mnem().ends_with('w') { 4 } else { 1 };
                    let signed = i.op().mnem().contains('s');
                    load.insert(d, (base, width, signed, index));
                }
                if let Some(d) = d {
                    val.remove(&d);
                }
            }
            // A table address the compiler spilled to the stack earlier.
            "ldr" if matches!(ops.get(1), Some(MemOffset { .. })) => {
                let slot = match ops.get(1) {
                    Some(MemOffset { reg: r, offset, .. }) => gpr_id(&Reg { reg: *r, arrspec: None }).map(|b| (b, imm_u64(offset) as i64)),
                    _ => None,
                };
                match (d, slot) {
                    (Some(d), Some((b @ (29 | 31), off))) => match stack_slot_value(code, addr, b, off) {
                        Some(v) => {
                            val.insert(d, v);
                        }
                        None => {
                            val.remove(&d);
                        }
                    },
                    (Some(d), _) => {
                        val.remove(&d);
                    }
                    _ => {}
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

    let Some(Table { label, base, width, signed, ext, index, scale }) = table else { return vec![] };
    let sx = |v: u64, bits: u32| ((v << (64 - bits)) as i64) >> (64 - bits);
    // The table's length, from the bounds check before it (`cmp wI, #max;
    // b.hi default`), when it's just before this block.
    let count = index.and_then(|idx| table_length(insns, block, idx));
    let mut targets = Vec::new();
    // Without a bound, read at most 256 entries (and stop at the first that
    // isn't code, below): data past the table can look like offsets, and every
    // false target splits a block.
    for i in 0..count.unwrap_or(256).min(4096) {
        let Some(raw) = image.read_uint(base + i * width as u64, width) else { break };
        let v = if signed { sx(raw, width * 8) as u64 } else { raw };
        // The add's extension decides how the entry is interpreted (sxth: a
        // signed halfword, so cases may lie before the base label).
        let entry = match ext {
            Shift::SXTB(_) => sx(v, 8),
            Shift::SXTH(_) => sx(v, 16),
            Shift::SXTW(_) => sx(v, 32),
            Shift::UXTB(_) => (v & 0xff) as i64,
            Shift::UXTH(_) => (v & 0xffff) as i64,
            Shift::UXTW(_) => (v & 0xffff_ffff) as i64,
            _ => v as i64,
        };
        let target = label.wrapping_add((entry * scale) as u64);
        if !image.is_code(target) {
            // Without a known length, the first non-code entry ends the table.
            if count.is_none() {
                break;
            }
            continue;
        }
        targets.push(target);
    }
    targets
}

/// A jump table's length from its bounds check: `cmp wI, #max` on the index
/// register just before a conditional branch that either falls into the
/// dispatch sequence (`b.hi default`) or jumps to it (`b.ls dispatch`).
fn table_length(insns: Insns, window: &[(u64, Instruction)], idx: u32) -> Option<u64> {
    use bad64::Operand::*;
    let (start, end) = (window[0].0, window[window.len() - 1].0);
    // The bound may be checked on the register the index was copied from
    // (`mov w2, w1; cmp w1, #max; b.ls ...; ldrh w0, [x0, w2, uxtw #1]`).
    let mut index = vec![idx];
    for (_, i) in insns.range(start.saturating_sub(64)..end + 4) {
        let ops = i.operands();
        if i.op().mnem() == "mov"
            && ops.first().and_then(gpr_id) == Some(idx)
            && let Some(src) = ops.get(1).and_then(gpr_id)
        {
            index.push(src);
        }
    }
    // Only a range check bounds the table: `cmp wI, #8; b.eq` in front of
    // a switch picks out one case (and may branch into the sequence too).
    let cmp_before = |branch: u64, cond: &str| -> Option<u64> {
        let extra = match cond {
            "b.ls" | "b.hi" | "b.le" | "b.gt" => 1, // index <= imm
            "b.lo" | "b.hs" | "b.cc" | "b.cs" | "b.lt" | "b.ge" => 0, // index < imm
            _ => return None,
        };
        let i = insns.get(&branch.checked_sub(4)?)?;
        let ops = i.operands();
        match (i.op().mnem(), ops.first().and_then(gpr_id), ops.get(1)) {
            ("cmp", Some(r), Some(Imm32 { imm, .. } | Imm64 { imm, .. })) if index.contains(&r) => Some(imm_u64(imm) + extra),
            _ => None,
        }
    };
    // A guard on the straight-line path, or just before it: the one nearest
    // the dispatch (earlier compares of the same register guard other code).
    let first = start.saturating_sub(4);
    for (a, i) in insns.range(first..end + 4).rev() {
        if i.op().mnem().starts_with("b.")
            && let Some(n) = cmp_before(a, i.op().mnem())
        {
            return Some(n);
        }
    }
    // A guard elsewhere that branches into the sequence.
    for (a, i) in insns.range(start.saturating_sub(0x10000)..end + 0x10000) {
        if i.op().mnem().starts_with("b.")
            && direct_target(&i).is_some_and(|t| (start..=end).contains(&t))
            && let Some(n) = cmp_before(a, i.op().mnem())
        {
            return Some(n);
        }
    }
    None
}

/// The value last stored to a frame slot (`[sp|fp, #off]`) before `from`:
/// finds the nearest preceding `str xS, [base, #off]` and resolves xS. A
/// linear, bounded look back: enough for the table addresses compilers hoist
/// out of loops and spill.
fn stack_slot_value(code: &Code, from: u64, base: u32, off: i64) -> Option<u64> {
    let insns = code.insns;
    use bad64::Operand::*;
    let mut pc = from;
    for _ in 0..4096 {
        pc = pc.checked_sub(4)?;
        let Some(i) = insns.get(&pc) else { continue };
        let ops = i.operands();
        if i.op().mnem() == "str"
            && let Some(MemOffset { reg: r, offset, .. }) = ops.get(1)
            && gpr_id(&Reg { reg: *r, arrspec: None }) == Some(base)
            && imm_u64(offset) as i64 == off
        {
            return register_value_before(code, pc, gpr_id(&ops[0])?, 0);
        }
    }
    None
}

/// The constant address register `r` holds just before `at`, if it was built
/// by adr, adrp, or adrp + add in the preceding instructions.
fn register_value_before(code: &Code, at: u64, r: u32, depth: u32) -> Option<u64> {
    use bad64::Operand::*;
    if depth > 4 {
        return None;
    }
    let insns = code.insns;
    let mut pc = at;
    for _ in 0..1024 {
        // At a merge point (including where the search starts), try the paths
        // that branch here first: the code just above may be unrelated
        // (another case of a switch, say).
        if let Some(sources) = code.incoming.get(&pc)
            && let Some(v) = sources.iter().take(8).find_map(|&s| register_value_before(code, s, r, depth + 1))
        {
            return Some(v);
        }
        let prev = pc.checked_sub(4)?;
        let i = insns.get(&prev)?;
        if is_terminal(&i) {
            // Only entered by branches, which were tried above.
            return None;
        }
        pc = prev;
        let ops = i.operands();
        let m = i.op().mnem();
        if ops.first().and_then(gpr_id) != Some(r) || m.starts_with("st") || matches!(m, "cmp" | "cmn" | "tst" | "cbz" | "cbnz" | "tbz" | "tbnz") {
            continue; // doesn't write r
        }
        return match (m, ops.get(1), ops.get(2)) {
            ("adr" | "adrp", Some(Label(imm)), _) => Some(imm_u64(imm)),
            ("add", Some(src), Some(Imm32 { imm, .. } | Imm64 { imm, .. })) => {
                Some(register_value_before(code, pc, gpr_id(src)?, depth + 1)?.wrapping_add(imm_u64(imm)))
            }
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn program(bytes: &[u8]) -> (&'static Image, Program<'static>) {
        // A program borrows its image; tests keep both (leaking the image).
        let image: &'static Image = Box::leak(Box::new(Image::load(bytes).unwrap()));
        (image, discover(image))
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
                if is_control(&i) {
                    if let Some(t) = direct_target(&i).filter(|t| prog.insns.contains_key(t)) {
                        assert!(prog.leaders.contains(&t), "branch at {pc:#x} to {t:#x}");
                    }
                    if !is_terminal(&i) && prog.insns.contains_key(&(pc + 4)) {
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
                if is_control(&i) || prog.leaders.contains(&pc) {
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
        let calls: Vec<u64> = reached.iter().copied().filter(|a| prog.insns.get(a).unwrap().op().mnem() == "bl").collect();
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
        assert!(prog.leaders.len() < prog.insns.iter().count() / 2, "blocks are a minority of instructions");
        // Words after an unconditional branch that nothing targets stay out.
        for (pc, i) in prog.insns.iter() {
            if is_terminal(&i) && !prog.leaders.contains(&(pc + 4)) {
                return;
            }
        }
        panic!("expected some unreached fall-through after a terminal instruction");
    }
}

/// The blocks of one page, for a page translated again at run time (see
/// lift's `page`), without discovering the whole program again: the blocks
/// the program's translation found there (`known`, which it wrote out with
/// `--leaders-out`), the addresses runs reached that it missed, and, as
/// discovery does around a missed target, the instruction after each one
/// that doesn't fall through; followed within the page. Every block is an
/// entry.
pub fn page_program<'a>(image: &'a Image, page: u64, known: &[u64], missed: &[u64]) -> Program<'a> {
    const PAGE: u64 = 0x1000;
    let insns = Insns::new(image);
    let in_page = |a: u64| (page..page + PAGE).contains(&a) && insns.contains_key(&a);
    let mut leaders: BTreeSet<u64> = known.iter().chain(missed).copied().filter(|&a| in_page(a)).collect();
    for (a, i) in insns.range(page..page + PAGE) {
        if is_terminal(&i) && in_page(a + 4) {
            leaders.insert(a + 4);
        }
    }
    let mut work: Vec<u64> = leaders.iter().copied().collect();
    while let Some(start) = work.pop() {
        let mut pc = start;
        while in_page(pc) {
            let Some(i) = insns.get(&pc) else { break };
            if is_control(&i) {
                if let Some(t) = direct_target(&i)
                    && in_page(t)
                    && leaders.insert(t)
                {
                    work.push(t);
                }
                if !is_terminal(&i) && in_page(pc + 4) && leaders.insert(pc + 4) {
                    work.push(pc + 4);
                }
                break;
            }
            pc += 4;
            if leaders.contains(&pc) {
                break;
            }
        }
    }
    let entries = leaders.clone();
    Program { insns, leaders, entries }
}
