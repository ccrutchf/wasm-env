//! SPIR-V naga's reader refuses, though valid, made what it reads: debug
//! instructions it doesn't know (`OpLine`, `OpNoLine`, `OpSourceContinued`,
//! `OpModuleProcessed`) and names that aren't UTF-8 dropped; decoration
//! groups made the decorations they stand for; loads', stores' and copies' memory
//! operands (`Aligned`, `Volatile`: hints) dropped; `OpUndef` in a function
//! a null constant (naga's reads one there twice); conservative depth's
//! execution modes (hints wgpu refuses) dropped; and a fetch from an image
//! that names no level given level 0 (WGSL's names one).

use std::collections::HashMap;

use super::{Inst, OP_CONSTANT, OP_FUNCTION, OP_TYPE_INT, inst};

const OP_UNDEF: u32 = 1;
const OP_SOURCE_CONTINUED: u32 = 2;
const OP_NAME: u32 = 5;
const OP_MEMBER_NAME: u32 = 6;
const OP_LINE: u32 = 8;
const OP_EXECUTION_MODE: u32 = 16;
const OP_CONSTANT_NULL: u32 = 46;
const OP_LOAD: u32 = 61;
const OP_STORE: u32 = 62;
const OP_COPY_MEMORY: u32 = 63;
const OP_DECORATE: u32 = 71;
const OP_MEMBER_DECORATE: u32 = 72;
const OP_DECORATION_GROUP: u32 = 73;
const OP_GROUP_DECORATE: u32 = 74;
const OP_GROUP_MEMBER_DECORATE: u32 = 75;
const OP_IMAGE_FETCH: u32 = 95;
const OP_NO_LINE: u32 = 317;
const OP_MODULE_PROCESSED: u32 = 330;
const MODE_DEPTH_GREATER: u32 = 14;
const MODE_DEPTH_UNCHANGED: u32 = 16;
const IMAGE_OPERANDS_LOD: u32 = 0x2;

/// Whether a name's string (its words from `from`) is UTF-8.
fn utf8(words: &[u32]) -> bool {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).is_ok()
}

/// The module, tidied (see the module's doc).
pub fn tidy(insts: Vec<Inst>, fresh: &mut dyn FnMut() -> u32) -> Vec<Inst> {
    // Decoration groups: each group's decorations.
    let groups: std::collections::HashSet<u32> = insts.iter().filter(|i| i.op == OP_DECORATION_GROUP).map(|i| i.words[0]).collect();
    let mut grouped: HashMap<u32, Vec<Vec<u32>>> = HashMap::new();
    for i in insts.iter().filter(|i| i.op == OP_DECORATE && groups.contains(&i.words[0])) {
        grouped.entry(i.words[0]).or_default().push(i.words[1..].to_vec());
    }
    // (A fetch with no level: an int's 0, made where there's none.)
    let fetches = insts.iter().any(|i| i.op == OP_IMAGE_FETCH && i.words.len() == 4);
    let mut made = Vec::new();
    let zero = fetches.then(|| {
        let int = insts.iter().find(|i| i.op == OP_TYPE_INT && i.words[1..] == [32, 1]).map(|i| i.words[0]).unwrap_or_else(|| {
            let t = fresh();
            made.push(inst(OP_TYPE_INT, &[t, 32, 1]));
            t
        });
        let z = fresh();
        made.push(inst(OP_CONSTANT, &[int, z, 0]));
        z
    });
    // (Undefined values in functions: null constants, before them.)
    let mut in_function = false;
    for i in &insts {
        if i.op == OP_FUNCTION {
            in_function = true;
        }
        if in_function && i.op == OP_UNDEF {
            made.push(inst(OP_CONSTANT_NULL, &[i.words[0], i.words[1]]));
        }
    }
    let mut out = Vec::with_capacity(insts.len());
    let mut functions = false;
    for i in insts {
        if i.op == OP_FUNCTION && !functions {
            functions = true;
            out.append(&mut made);
        }
        match i.op {
            OP_LINE | OP_NO_LINE | OP_SOURCE_CONTINUED | OP_MODULE_PROCESSED => {}
            OP_NAME if !utf8(&i.words[1..]) => {}
            OP_MEMBER_NAME if !utf8(&i.words[2..]) => {}
            OP_DECORATION_GROUP => {}
            OP_DECORATE if groups.contains(&i.words[0]) => {}
            OP_GROUP_DECORATE => {
                for &target in &i.words[1..] {
                    for d in grouped.get(&i.words[0]).into_iter().flatten() {
                        out.push(Inst { op: OP_DECORATE, words: [&[target][..], d].concat() });
                    }
                }
            }
            OP_GROUP_MEMBER_DECORATE => {
                for pair in i.words[1..].chunks(2) {
                    for d in grouped.get(&i.words[0]).into_iter().flatten() {
                        out.push(Inst { op: OP_MEMBER_DECORATE, words: [pair, d].concat() });
                    }
                }
            }
            OP_LOAD if i.words.len() > 3 => out.push(Inst { op: i.op, words: i.words[..3].to_vec() }),
            OP_STORE | OP_COPY_MEMORY if i.words.len() > 2 => out.push(Inst { op: i.op, words: i.words[..2].to_vec() }),
            OP_UNDEF if functions => {}
            OP_EXECUTION_MODE if (MODE_DEPTH_GREATER..=MODE_DEPTH_UNCHANGED).contains(&i.words[1]) => {}
            OP_IMAGE_FETCH if i.words.len() == 4 => {
                let mut words = i.words.clone();
                words.extend([IMAGE_OPERANDS_LOD, zero.unwrap()]);
                out.push(Inst { op: i.op, words });
            }
            _ => out.push(i),
        }
    }
    out
}
