//! SPIR-V made ready for naga (vulkan.rs): its combined image samplers
//! (GLSL's `sampler2D`), which naga doesn't read, split into an image and a
//! sampler, and its bindings renumbered to leave room for the samplers.
//!
//! Vulkan binding `b` becomes binding `2b`; a combined image sampler's
//! sampler is `2b + 1` (`binding`). A load of a combined image sampler
//! becomes loads of its image and its sampler, put together again by
//! `OpSampledImage`, which naga does read.
//!
//! And writes of `gl_PointSize`, which WGSL hasn't (WebGPU's points are one
//! pixel), are dropped: naga leaves out the builtins nothing touches.

use std::collections::HashMap;

use crate::ir::Res;

const OP_ENTRY_POINT: u32 = 15;
const OP_TYPE_SAMPLER: u32 = 26;
const OP_TYPE_SAMPLED_IMAGE: u32 = 27;
const OP_TYPE_POINTER: u32 = 32;
const OP_VARIABLE: u32 = 59;
const OP_LOAD: u32 = 61;
const OP_STORE: u32 = 62;
const OP_ACCESS_CHAIN: u32 = 65;
const OP_IN_BOUNDS_ACCESS_CHAIN: u32 = 66;
const OP_CONSTANT: u32 = 43;
const OP_MEMBER_DECORATE: u32 = 72;
const DECORATION_BUILTIN: u32 = 11;
const BUILTIN_POINT_SIZE: u32 = 1;
const OP_DECORATE: u32 = 71;
const OP_SAMPLED_IMAGE: u32 = 86;
const DECORATION_BINDING: u32 = 33;
const DECORATION_DESCRIPTOR_SET: u32 = 34;
const UNIFORM_CONSTANT: u32 = 0;

/// The WebGPU binding for a Vulkan binding (`sampler`: a combined image
/// sampler's sampler).
pub fn binding(vulkan: u32, sampler: bool) -> u32 {
    2 * vulkan + sampler as u32
}

/// The Vulkan binding a WebGPU binding came from, and whether it's a
/// combined image sampler's sampler.
pub fn vulkan_binding(wgpu: u32) -> (u32, bool) {
    (wgpu / 2, wgpu % 2 == 1)
}

/// An instruction: its opcode and operands.
struct Inst {
    op: u32,
    words: Vec<u32>,
}

fn inst(op: u32, operands: &[u32]) -> Inst {
    Inst { op, words: operands.to_vec() }
}

const OP_TYPE_INT: u32 = 21;
const OP_TYPE_FLOAT: u32 = 22;
const OP_TYPE_VECTOR: u32 = 23;
const OP_TYPE_IMAGE: u32 = 25;
const OP_FUNCTION: u32 = 54;
const OP_VECTOR_SHUFFLE: u32 = 79;
const OP_IMAGE_FETCH: u32 = 95;
const OP_IMAGE_READ: u32 = 98;
const OP_CONVERT_F_TO_S: u32 = 110;
const OP_I_ADD: u32 = 128;
const DIM_2D: u32 = 1;
const DIM_SUBPASS_DATA: u32 = 6;
const STORAGE_INPUT: u32 = 1;
const BUILTIN_FRAG_COORD: u32 = 15;
const EXECUTION_FRAGMENT: u32 = 4;
const IMAGE_OPERANDS_LOD: u32 = 0x2;

/// Input attachments (GLSL's `subpassInput`), which naga doesn't read, as
/// ordinary 2D images: each `subpassLoad` (`OpImageRead`, at an offset from
/// the fragment) a fetch at the fragment's position plus it. (The driver
/// binds an input attachment as a sampled image of its view.)
fn subpass_inputs(insts: Vec<Inst>, fresh: &mut impl FnMut() -> u32) -> Vec<Inst> {
    let subpass: std::collections::HashSet<u32> = insts.iter().filter(|i| i.op == OP_TYPE_IMAGE && i.words[2] == DIM_SUBPASS_DATA).map(|i| i.words[0]).collect();
    if subpass.is_empty() {
        return insts;
    }
    // Images loaded from input attachments.
    let images: std::collections::HashSet<u32> = insts.iter().filter(|i| i.op == OP_LOAD && subpass.contains(&i.words[0])).map(|i| i.words[1]).collect();
    // What the fetches need, found or made: f32, vec4 and vec2 of it, i32,
    // its vec2 and 0, an Input pointer to vec4, gl_FragCoord.
    let mut made: Vec<Inst> = Vec::new();
    let mut find = |made: &mut Vec<Inst>, op: u32, operands: &[u32]| -> u32 {
        if let Some(i) = insts.iter().chain(made.iter()).find(|i| i.op == op && i.words[1..] == *operands) {
            return i.words[0];
        }
        let id = fresh();
        made.push(Inst { op, words: [&[id], operands].concat() });
        id
    };
    let float = find(&mut made, OP_TYPE_FLOAT, &[32]);
    let vec4 = find(&mut made, OP_TYPE_VECTOR, &[float, 4]);
    let vec2 = find(&mut made, OP_TYPE_VECTOR, &[float, 2]);
    let int = find(&mut made, OP_TYPE_INT, &[32, 1]);
    let ivec2 = find(&mut made, OP_TYPE_VECTOR, &[int, 2]);
    let input_vec4 = find(&mut made, OP_TYPE_POINTER, &[STORAGE_INPUT, vec4]);
    // (OpConstant's operands put the type first: found by hand.)
    let zero = match insts.iter().find(|i| i.op == OP_CONSTANT && i.words[0] == int && i.words[2] == 0) {
        Some(i) => i.words[1],
        None => {
            let id = fresh();
            made.push(inst(OP_CONSTANT, &[int, id, 0]));
            id
        }
    };
    let decorated: Vec<u32> = insts.iter().filter(|i| i.op == OP_DECORATE && i.words[1..] == [DECORATION_BUILTIN, BUILTIN_FRAG_COORD]).map(|i| i.words[0]).collect();
    let (frag_coord, new_var) = match insts.iter().find(|i| i.op == OP_VARIABLE && decorated.contains(&i.words[1])) {
        Some(i) => (i.words[1], false),
        None => {
            let id = fresh();
            made.push(inst(OP_VARIABLE, &[input_vec4, id, STORAGE_INPUT]));
            (id, true)
        }
    };
    let mut out = Vec::with_capacity(insts.len() + made.len() + 8);
    let mut placed = false;
    let mut decoration_placed = !new_var;
    for i in insts {
        // (A new variable's decoration among the annotations: before the
        // first type.)
        if !decoration_placed && (19..=39).contains(&i.op) {
            out.push(inst(OP_DECORATE, &[frag_coord, DECORATION_BUILTIN, BUILTIN_FRAG_COORD]));
            decoration_placed = true;
        }
        // (What was made, after every global: before the first function.)
        if !placed && i.op == OP_FUNCTION {
            out.append(&mut made);
            placed = true;
        }
        match i.op {
            OP_TYPE_IMAGE if subpass.contains(&i.words[0]) => {
                let mut w = i.words.clone();
                w[2] = DIM_2D;
                w[6] = 1; // sampled
                out.push(Inst { op: OP_TYPE_IMAGE, words: w });
            }
            // (A fragment shader's interface lists gl_FragCoord.)
            OP_ENTRY_POINT if new_var && i.words[0] == EXECUTION_FRAGMENT => {
                let mut w = i.words.clone();
                w.push(frag_coord);
                out.push(Inst { op: OP_ENTRY_POINT, words: w });
            }
            OP_IMAGE_READ if images.contains(&i.words[2]) => {
                let (ty, result, image, offset) = (i.words[0], i.words[1], i.words[2], i.words[3]);
                let (fc, xy, ixy, at) = (fresh(), fresh(), fresh(), fresh());
                out.push(inst(OP_LOAD, &[vec4, fc, frag_coord]));
                out.push(inst(OP_VECTOR_SHUFFLE, &[vec2, xy, fc, fc, 0, 1]));
                out.push(inst(OP_CONVERT_F_TO_S, &[ivec2, ixy, xy]));
                out.push(inst(OP_I_ADD, &[ivec2, at, ixy, offset]));
                // (Multisampled: its sample operand kept; else level 0.)
                let operands = if i.words.len() > 4 { i.words[4..].to_vec() } else { vec![IMAGE_OPERANDS_LOD, zero] };
                out.push(Inst { op: OP_IMAGE_FETCH, words: [vec![ty, result, image, at], operands].concat() });
            }
            _ => out.push(i),
        }
    }
    out
}

const OP_COPY_OBJECT: u32 = 83;
const OP_IMAGE: u32 = 100;
const DIM_1D: u32 = 0;
const DIM_BUFFER: u32 = 5;

/// Texel buffers (GLSL's `samplerBuffer`, `textureBuffer`, `imageBuffer`),
/// which WGSL hasn't, as 1D images (the runtime binds a texel buffer view
/// as a 1D texture of its texels: vulkan.rs, `TexelView`). A `samplerBuffer`
/// has no sampler (its descriptor holds none): its variable becomes the
/// image's, its `OpImage` a copy. A fetch from a buffer has no level: it
/// gets level 0.
fn texel_buffers(insts: Vec<Inst>, fresh: &mut impl FnMut() -> u32) -> Vec<Inst> {
    use std::collections::{HashMap, HashSet};
    let buffers: HashSet<u32> = insts.iter().filter(|i| i.op == OP_TYPE_IMAGE && i.words[2] == DIM_BUFFER).map(|i| i.words[0]).collect();
    if buffers.is_empty() {
        return insts;
    }
    let sampled: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_TYPE_SAMPLED_IMAGE && buffers.contains(&i.words[1])).map(|i| (i.words[0], i.words[1])).collect();
    // Image values from texel buffers (loads, and what OpImage extracts),
    // found in order.
    let mut values: HashSet<u32> = HashSet::new();
    for i in &insts {
        if i.op == OP_LOAD && (buffers.contains(&i.words[0]) || sampled.contains_key(&i.words[0])) {
            values.insert(i.words[1]);
        }
        if i.op == OP_IMAGE && values.contains(&i.words[2]) {
            values.insert(i.words[1]);
        }
    }
    let int = insts.iter().find(|i| i.op == OP_TYPE_INT && i.words[1..] == [32, 1]).map(|i| i.words[0]);
    let zero = int.and_then(|int| insts.iter().find(|i| i.op == OP_CONSTANT && i.words[0] == int && i.words[2] == 0).map(|i| i.words[1]));
    let mut made = Vec::new();
    let zero = match zero {
        Some(zero) => zero,
        None => {
            let int = int.unwrap_or_else(|| {
                let id = fresh();
                made.push(inst(OP_TYPE_INT, &[id, 32, 1]));
                id
            });
            let id = fresh();
            made.push(inst(OP_CONSTANT, &[int, id, 0]));
            id
        }
    };

    let mut out = Vec::with_capacity(insts.len() + 2);
    let mut placed = false;
    for i in insts {
        if !placed && i.op == OP_FUNCTION {
            out.append(&mut made);
            placed = true;
        }
        match i.op {
            OP_TYPE_IMAGE if buffers.contains(&i.words[0]) => {
                let mut w = i.words.clone();
                w[2] = DIM_1D;
                out.push(Inst { op: OP_TYPE_IMAGE, words: w });
            }
            // (Pointers to a samplerBuffer point to its image.)
            OP_TYPE_POINTER if sampled.contains_key(&i.words[2]) => out.push(inst(OP_TYPE_POINTER, &[i.words[0], i.words[1], sampled[&i.words[2]]])),
            OP_LOAD if sampled.contains_key(&i.words[0]) => {
                let mut w = i.words.clone();
                w[0] = sampled[&i.words[0]];
                out.push(Inst { op: OP_LOAD, words: w });
            }
            OP_IMAGE if values.contains(&i.words[2]) => out.push(inst(OP_COPY_OBJECT, &i.words)),
            OP_IMAGE_FETCH if values.contains(&i.words[2]) && i.words.len() == 4 => {
                out.push(Inst { op: OP_IMAGE_FETCH, words: [i.words.clone(), vec![IMAGE_OPERANDS_LOD, zero]].concat() });
            }
            _ => out.push(i),
        }
    }
    out
}

const OP_IS_NAN: u32 = 156;
const OP_IS_INF: u32 = 157;
const OP_BITCAST: u32 = 124;
const OP_BITWISE_AND: u32 = 199;
const OP_I_EQUAL: u32 = 170;
const OP_U_GREATER_THAN: u32 = 172;

/// `isnan` and `isinf`, which WGSL hasn't, by their bits (a compiler may
/// take `x != x` for false): the float's bits but its sign above, or at,
/// an infinity's. (32-bit floats, scalars or vectors.)
fn nan_tests(insts: Vec<Inst>, fresh: &mut impl FnMut() -> u32) -> Vec<Inst> {
    if !insts.iter().any(|i| i.op == OP_IS_NAN || i.op == OP_IS_INF) {
        return insts;
    }
    // The (bool) result type's lanes: a vector's count, or 1.
    let lanes: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_TYPE_VECTOR).map(|i| (i.words[0], i.words[2])).collect();
    let mut made: Vec<Inst> = Vec::new();
    let find = |made: &mut Vec<Inst>, fresh: &mut dyn FnMut() -> u32, op: u32, operands: &[u32]| -> u32 {
        if let Some(i) = insts.iter().chain(made.iter()).find(|i| i.op == op && i.words[1..] == *operands) {
            return i.words[0];
        }
        let id = fresh();
        made.push(Inst { op, words: [&[id], operands].concat() });
        id
    };
    let uint = find(&mut made, &mut *fresh, OP_TYPE_INT, &[32, 0]);
    let constant = |made: &mut Vec<Inst>, fresh: &mut dyn FnMut() -> u32, ty: u32, v: u32| -> u32 {
        if let Some(i) = insts.iter().chain(made.iter()).find(|i| i.op == OP_CONSTANT && i.words[0] == ty && i.words[2..] == [v]) {
            return i.words[1];
        }
        let id = fresh();
        made.push(inst(OP_CONSTANT, &[ty, id, v]));
        id
    };
    // Per lane count: the uint type (or its vector) and the two constants
    // (splatted for a vector).
    let (abs, inf) = (constant(&mut made, &mut *fresh, uint, 0x7fff_ffff), constant(&mut made, &mut *fresh, uint, 0x7f80_0000));
    let counts: std::collections::BTreeSet<u32> =
        insts.iter().filter(|i| i.op == OP_IS_NAN || i.op == OP_IS_INF).map(|i| lanes.get(&i.words[0]).copied().unwrap_or(1)).filter(|&n| n > 1).collect();
    let vectors: Vec<(u32, u32)> = counts.into_iter().map(|n| (n, find(&mut made, &mut *fresh, OP_TYPE_VECTOR, &[uint, n]))).collect();

    let mut kinds: HashMap<u32, (u32, u32, u32)> = HashMap::from([(1, (uint, abs, inf))]);
    for (n, ty) in vectors {
        let (a, b) = (fresh(), fresh());
        made.push(Inst { op: OP_CONSTANT_COMPOSITE, words: [vec![ty, a], vec![abs; n as usize]].concat() });
        made.push(Inst { op: OP_CONSTANT_COMPOSITE, words: [vec![ty, b], vec![inf; n as usize]].concat() });
        kinds.insert(n, (ty, a, b));
    }
    let mut out = Vec::with_capacity(insts.len() + made.len());
    let mut placed = false;
    for i in insts {
        if !placed && i.op == OP_FUNCTION {
            out.append(&mut made);
            placed = true;
        }
        match i.op {
            OP_IS_NAN | OP_IS_INF => {
                let (ty, result, x) = (i.words[0], i.words[1], i.words[2]);
                let n = lanes.get(&ty).copied().unwrap_or(1);
                let (uty, abs, inf) = kinds[&n];

                let (bits, magnitude) = (fresh(), fresh());
                out.push(inst(OP_BITCAST, &[uty, bits, x]));
                out.push(inst(OP_BITWISE_AND, &[uty, magnitude, bits, abs]));
                out.push(inst(if i.op == OP_IS_NAN { OP_U_GREATER_THAN } else { OP_I_EQUAL }, &[ty, result, magnitude, inf]));
            }
            _ => out.push(i),
        }
    }
    out
}

/// A module's header (five words) and instructions.
fn parse(spirv: &[u8]) -> Res<(Vec<u32>, Vec<Inst>)> {
    if spirv.len() % 4 != 0 || spirv.len() < 20 {
        return Err("SPIR-V: not a whole number of words".into());
    }
    let words: Vec<u32> = spirv.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    if words[0] != 0x0723_0203 {
        return Err("SPIR-V: no magic number (or not little-endian)".into());
    }
    let mut insts = Vec::new();
    let mut at = 5;
    while at < words.len() {
        let (n, op) = ((words[at] >> 16) as usize, words[at] & 0xffff);
        if n == 0 || at + n > words.len() {
            return Err(format!("SPIR-V: an instruction of {n} words at word {at}"));
        }
        insts.push(Inst { op, words: words[at + 1..at + n].to_vec() });
        at += n;
    }
    Ok((words[..5].to_vec(), insts))
}

fn emit(header: Vec<u32>, insts: &[Inst]) -> Vec<u8> {
    let mut words = header;
    for i in insts {
        words.push(((i.words.len() as u32 + 1) << 16) | i.op);
        words.extend(&i.words);
    }
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

const OP_TYPE_BOOL: u32 = 20;
const OP_CONSTANT_TRUE: u32 = 41;
const OP_CONSTANT_FALSE: u32 = 42;
const OP_CONSTANT_COMPOSITE: u32 = 44;
const OP_SPEC_CONSTANT_TRUE: u32 = 48;
const OP_SPEC_CONSTANT_FALSE: u32 = 49;
const OP_SPEC_CONSTANT: u32 = 50;
const OP_SPEC_CONSTANT_COMPOSITE: u32 = 51;
const OP_SPEC_CONSTANT_OP: u32 = 52;
const DECORATION_SPEC_ID: u32 = 1;

/// Whether a module computes with specialization constants
/// (`OpSpecConstantOp`), which naga doesn't read: it's specialized first
/// (`specialize`).
pub fn computes_with_spec_constants(spirv: &[u8]) -> bool {
    parse(spirv).is_ok_and(|(_, insts)| insts.iter().any(|i| i.op == OP_SPEC_CONSTANT_OP))
}

/// The module with its specialization constants made constants: those
/// `values` give (SpecId, bits) theirs, the rest their defaults, and what
/// `OpSpecConstantOp`s compute from them computed.
pub fn specialize(spirv: &[u8], values: &[(u32, u64)]) -> Res<Vec<u8>> {
    let (header, insts) = parse(spirv)?;
    let spec_ids: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_DECORATE && i.words.get(1) == Some(&DECORATION_SPEC_ID)).map(|i| (i.words[0], i.words[2])).collect();
    // Scalar types: bool, or an integer's width and signedness.
    let bools: std::collections::HashSet<u32> = insts.iter().filter(|i| i.op == OP_TYPE_BOOL).map(|i| i.words[0]).collect();
    let ints: HashMap<u32, (u32, bool)> = insts.iter().filter(|i| i.op == OP_TYPE_INT).map(|i| (i.words[0], (i.words[1], i.words[2] == 1))).collect();
    // Each scalar constant's value and type, as they're found.
    let mut known: HashMap<u32, (u64, u32)> = HashMap::new();
    let value_of = |id: u32| values.iter().rev().find(|(s, _)| spec_ids.get(&id) == Some(s)).map(|(_, v)| *v);
    let mask = |ty: u32, v: u64| match ints.get(&ty) {
        Some(&(w, _)) if w < 64 => v & ((1u64 << w) - 1),
        _ => v,
    };
    // (A value as a signed number of its type's width.)
    let signed = |ty: u32, v: u64| match ints.get(&ty) {
        Some(&(w, _)) if w < 64 => ((v << (64 - w)) as i64) >> (64 - w),
        _ => v as i64,
    };
    let constant = |ty: u32, id: u32, v: u64| -> Inst {
        if bools.contains(&ty) {
            inst(if v != 0 { OP_CONSTANT_TRUE } else { OP_CONSTANT_FALSE }, &[ty, id])
        } else if ints.get(&ty).is_some_and(|&(w, _)| w > 32) {
            inst(OP_CONSTANT, &[ty, id, v as u32, (v >> 32) as u32])
        } else {
            inst(OP_CONSTANT, &[ty, id, v as u32])
        }
    };
    let mut out = Vec::with_capacity(insts.len());
    for i in insts {
        match i.op {
            OP_CONSTANT if i.words.len() >= 3 => {
                let v = i.words[2] as u64 | i.words.get(3).map_or(0, |&h| (h as u64) << 32);
                known.insert(i.words[1], (v, i.words[0]));
                out.push(i);
            }
            OP_CONSTANT_TRUE | OP_CONSTANT_FALSE => {
                known.insert(i.words[1], ((i.op == OP_CONSTANT_TRUE) as u64, i.words[0]));
                out.push(i);
            }
            OP_SPEC_CONSTANT_TRUE | OP_SPEC_CONSTANT_FALSE => {
                let (ty, id) = (i.words[0], i.words[1]);
                let v = value_of(id).map_or((i.op == OP_SPEC_CONSTANT_TRUE) as u64, |v| (v != 0) as u64);
                known.insert(id, (v, ty));
                out.push(constant(ty, id, v));
            }
            // (A constant's words, its value the one given, if any: an
            // integer's or a float's bits.)
            OP_SPEC_CONSTANT => {
                let (ty, id) = (i.words[0], i.words[1]);
                let mut words = i.words.clone();
                if let Some(v) = value_of(id) {
                    words[2] = mask(ty, v) as u32;
                    if words.len() > 3 {
                        words[3] = (mask(ty, v) >> 32) as u32;
                    }
                }
                known.insert(id, (words[2] as u64 | words.get(3).map_or(0, |&h| (h as u64) << 32), ty));
                out.push(Inst { op: OP_CONSTANT, words });
            }
            OP_SPEC_CONSTANT_COMPOSITE => out.push(Inst { op: OP_CONSTANT_COMPOSITE, words: i.words }),
            OP_SPEC_CONSTANT_OP => {
                let (ty, id, op) = (i.words[0], i.words[1], i.words[2]);
                let arg = |k: usize| -> Res<(u64, u32)> {
                    let a = *i.words.get(3 + k).ok_or("SPIR-V: OpSpecConstantOp missing an operand")?;
                    known.get(&a).copied().ok_or_else(|| format!("SPIR-V: OpSpecConstantOp of a value that isn't a scalar constant ({a})"))
                };
                let (a, at) = arg(0)?;
                let b = || arg(1);
                let (sa, ua) = (signed(at, a), a);
                let v: u64 = match op {
                    // (Select: its condition, then its two values.)
                    169 => {
                        if a != 0 {
                            b()?.0
                        } else {
                            arg(2)?.0
                        }
                    }
                    126 => sa.wrapping_neg() as u64,
                    200 => !ua,
                    168 => (a == 0) as u64,
                    113 => ua,                // UConvert
                    114 => sa as u64,         // SConvert
                    _ => {
                        let (b, _) = b()?;
                        let sb = signed(at, b);
                        match op {
                            128 => ua.wrapping_add(b),
                            130 => ua.wrapping_sub(b),
                            132 => ua.wrapping_mul(b),
                            134 => ua.checked_div(mask(at, b)).unwrap_or(0),
                            135 => sa.checked_div(sb).unwrap_or(0) as u64,
                            137 => ua.checked_rem(mask(at, b)).unwrap_or(0),
                            138 => sa.checked_rem(sb).unwrap_or(0) as u64,
                            139 => sa.checked_rem_euclid(sb).map_or(0, |r| if sb < 0 && r != 0 { r + sb } else { r }) as u64,
                            194 => mask(at, ua) >> (b & 63),
                            195 => (sa >> (b & 63)) as u64,
                            196 => ua << (b & 63),
                            197 => ua | b,
                            198 => ua ^ b,
                            199 => ua & b,
                            164 => (a == b) as u64,
                            165 => (a != b) as u64,
                            166 => (a != 0 || b != 0) as u64,
                            167 => (a != 0 && b != 0) as u64,
                            170 => (mask(at, ua) == mask(at, b)) as u64,
                            171 => (mask(at, ua) != mask(at, b)) as u64,
                            172 => (mask(at, ua) > mask(at, b)) as u64,
                            173 => (sa > sb) as u64,
                            174 => (mask(at, ua) >= mask(at, b)) as u64,
                            175 => (sa >= sb) as u64,
                            176 => (mask(at, ua) < mask(at, b)) as u64,
                            177 => (sa < sb) as u64,
                            178 => (mask(at, ua) <= mask(at, b)) as u64,
                            179 => (sa <= sb) as u64,
                            op => return Err(format!("SPIR-V: OpSpecConstantOp of opcode {op}")),
                        }
                    }
                };
                let v = mask(ty, v);
                known.insert(id, (v, ty));
                out.push(constant(ty, id, v));
            }
            // (No longer specialization constants: no SpecIds.)
            OP_DECORATE if i.words.get(1) == Some(&DECORATION_SPEC_ID) => {}
            _ => out.push(i),
        }
    }
    Ok(emit(header, &out))
}

/// The module, rewritten (see the module's doc).
pub fn prepare(spirv: &[u8]) -> Res<Vec<u8>> {
    let (mut header, insts) = parse(spirv)?;
    let mut next = header[3];
    let mut fresh = || {
        next += 1;
        next - 1
    };
    let insts = texel_buffers(insts, &mut fresh);
    let insts = subpass_inputs(insts, &mut fresh);
    let insts = nan_tests(insts, &mut fresh);

    // Combined image sampler types (to their image type), pointers to them
    // (to the type), and variables of those (to the image type).
    let sampled: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_TYPE_SAMPLED_IMAGE).map(|i| (i.words[0], i.words[1])).collect();
    let pointers: HashMap<u32, u32> =
        insts.iter().filter(|i| i.op == OP_TYPE_POINTER && i.words[1] == UNIFORM_CONSTANT && sampled.contains_key(&i.words[2])).map(|i| (i.words[0], i.words[2])).collect();
    let combined: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_VARIABLE && pointers.contains_key(&i.words[0])).map(|i| (i.words[1], sampled[&pointers[&i.words[0]]])).collect();
    // The sampler type (made, if the module has none), a pointer to it,
    // and per combined variable its image's pointer type and its sampler.
    let existing_sampler = insts.iter().find(|i| i.op == OP_TYPE_SAMPLER).map(|i| i.words[0]);
    let sampler_type = existing_sampler.unwrap_or_else(&mut fresh);
    let sampler_pointer = fresh();
    let parts: HashMap<u32, (u32, u32)> = combined.keys().map(|&v| (v, (fresh(), fresh()))).collect();

    // gl_PointSize: a variable decorated so, or a member of a block (the
    // block's type, the member); pointers to it (those variables, and
    // access chains into the member), whose stores go.
    let point_vars: Vec<u32> =
        insts.iter().filter(|i| i.op == OP_DECORATE && i.words[1..] == [DECORATION_BUILTIN, BUILTIN_POINT_SIZE]).map(|i| i.words[0]).collect();
    let point_members: HashMap<u32, u32> =
        insts.iter().filter(|i| i.op == OP_MEMBER_DECORATE && i.words[2..] == [DECORATION_BUILTIN, BUILTIN_POINT_SIZE]).map(|i| (i.words[0], i.words[1])).collect();
    let pointee: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_TYPE_POINTER).map(|i| (i.words[0], i.words[2])).collect();
    let var_type: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_VARIABLE).filter_map(|i| Some((i.words[1], *pointee.get(&i.words[0])?))).collect();
    let constants: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_CONSTANT && i.words.len() == 3).map(|i| (i.words[1], i.words[2])).collect();
    let mut points: std::collections::HashSet<u32> = point_vars.iter().copied().collect();
    for i in &insts {
        if (i.op == OP_ACCESS_CHAIN || i.op == OP_IN_BOUNDS_ACCESS_CHAIN) && i.words.len() >= 4 {
            let member = var_type.get(&i.words[2]).and_then(|t| point_members.get(t));
            if member.is_some() && member == constants.get(&i.words[3]) {
                points.insert(i.words[1]);
            }
        }
    }
    let point_chains: std::collections::HashSet<u32> = points.iter().filter(|p| !point_vars.contains(p)).copied().collect();

    let mut out: Vec<Inst> = Vec::with_capacity(insts.len());
    let mut declared = false;
    for i in insts {
        match i.op {
            // (Points: stores to gl_PointSize, the access chains they went
            // through, and a variable of its own with its decorations.)
            OP_STORE if points.contains(&i.words[0]) => {}
            OP_ACCESS_CHAIN | OP_IN_BOUNDS_ACCESS_CHAIN if point_chains.contains(&i.words[1]) => {}
            OP_VARIABLE if point_vars.contains(&i.words[1]) => {}
            OP_DECORATE if point_vars.contains(&i.words[0]) => {}
            // (What it needs declared before the first combined variable.)
            OP_VARIABLE if combined.contains_key(&i.words[1]) => {
                let v = i.words[1];
                let (image_pointer, sampler) = parts[&v];
                if !declared {
                    if existing_sampler.is_none() {
                        out.push(inst(OP_TYPE_SAMPLER, &[sampler_type]));
                    }
                    out.push(inst(OP_TYPE_POINTER, &[sampler_pointer, UNIFORM_CONSTANT, sampler_type]));
                    declared = true;
                }
                out.push(inst(OP_TYPE_POINTER, &[image_pointer, UNIFORM_CONSTANT, combined[&v]]));
                out.push(inst(OP_VARIABLE, &[image_pointer, v, UNIFORM_CONSTANT]));
                out.push(inst(OP_VARIABLE, &[sampler_pointer, sampler, UNIFORM_CONSTANT]));
            }
            OP_LOAD if combined.contains_key(&i.words[2]) => {
                let (ty, result, v) = (i.words[0], i.words[1], i.words[2]);
                let (image, sampler) = (fresh(), fresh());
                out.push(inst(OP_LOAD, &[combined[&v], image, v]));
                out.push(inst(OP_LOAD, &[sampler_type, sampler, parts[&v].1]));
                out.push(inst(OP_SAMPLED_IMAGE, &[ty, result, image, sampler]));
            }
            // Bindings doubled; a combined variable's set and binding given
            // to its sampler too.
            OP_DECORATE if i.words[1] == DECORATION_BINDING || i.words[1] == DECORATION_DESCRIPTOR_SET => {
                let (target, kind, value) = (i.words[0], i.words[1], i.words[2]);
                let binding_of = |sampler| if kind == DECORATION_BINDING { binding(value, sampler) } else { value };
                out.push(inst(OP_DECORATE, &[target, kind, binding_of(false)]));
                if let Some(&(_, sampler)) = parts.get(&target) {
                    out.push(inst(OP_DECORATE, &[sampler, kind, binding_of(true)]));
                }
            }
            // (A SPIR-V 1.4 interface lists every global the entry point
            // uses: the sampler too, and gl_PointSize's variable no more.)
            OP_ENTRY_POINT => {
                // (Its execution model, its function, its name (a string,
                // through the word with its NUL), then the interface.)
                let name_end = 2 + i.words[2..].iter().position(|w| w.to_le_bytes().contains(&0)).map_or(i.words.len() - 2, |k| k + 1);
                let mut words: Vec<u32> = i.words[..name_end].iter().chain(i.words[name_end..].iter().filter(|w| !point_vars.contains(w))).copied().collect();
                words.extend(i.words[name_end..].iter().filter_map(|w| parts.get(w).map(|p| p.1)));
                out.push(Inst { op: OP_ENTRY_POINT, words });
            }
            _ => out.push(i),
        }
    }
    header[3] = next;
    Ok(emit(header, &out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPIR-V of WGSL (naga's writer; glslang's combined image samplers are
    /// tested end to end, tests/arm64/vulkan.c).
    fn spirv(wgsl: &str) -> Vec<u8> {
        let module = naga::front::wgsl::parse_str(wgsl).unwrap();
        let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&module).unwrap();
        naga::back::spv::write_vec(&module, &info, &Default::default(), None).unwrap().iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn bindings_are_doubled() {
        let s = spirv(
            "@group(0) @binding(3) var<storage, read_write> a: array<u32>;
@group(1) @binding(0) var t: texture_2d<f32>;
@group(1) @binding(1) var s: sampler;
@compute @workgroup_size(1) fn main() { a[0] = u32(textureSampleLevel(t, s, vec2(0.5), 0.0).x); }",
        );
        let m = naga::front::spv::parse_u8_slice(&prepare(&s).unwrap(), &Default::default()).unwrap();
        let mut got: Vec<(u32, u32)> = m.global_variables.iter().filter_map(|(_, g)| g.binding.as_ref().map(|b| (b.group, b.binding))).collect();
        got.sort();
        assert_eq!(got, [(0, 6), (1, 0), (1, 2)]);
        assert_eq!(vulkan_binding(binding(5, true)), (5, true));
    }

    #[test]
    fn what_isnt_spirv_is_refused() {
        assert!(prepare(&[1, 2, 3]).is_err());
        assert!(prepare(&[0; 24]).is_err());
        // (An instruction running past the end: five words, one there.)
        let mut s = spirv("@compute @workgroup_size(1) fn main() {}");
        s.extend(((5u32 << 16) | 1).to_le_bytes());
        assert!(prepare(&s).is_err());
    }
}
