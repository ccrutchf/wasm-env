//! 1D images as 2D ones a row high (the runtime makes a 1D image a 2D
//! texture, WebGPU's 1D textures having no levels, layers or attachments:
//! vulkan.rs, `image`). Each 1D image type is 2D; a coordinate gets a
//! second component, 0.5 where it's sampled (the row's middle) and 0 where
//! it's fetched, read or written, before an array's layer; gradients and
//! offsets get a second one of 0; a size asked for is the width (and the
//! layers), not the height.

use std::collections::{HashMap, HashSet};

use super::{Inst, OP_CONSTANT, OP_FUNCTION, OP_TYPE_INT, OP_TYPE_VECTOR, inst};

const OP_TYPE_FLOAT: u32 = 22;
const OP_TYPE_IMAGE: u32 = 25;
const OP_TYPE_SAMPLED_IMAGE: u32 = 27;
const OP_CONSTANT_COMPOSITE: u32 = 44;
const OP_VECTOR_SHUFFLE: u32 = 79;
const OP_COMPOSITE_CONSTRUCT: u32 = 80;
const OP_COMPOSITE_EXTRACT: u32 = 81;
const OP_BITCAST: u32 = 124;
const OP_F_MUL: u32 = 133;
const DIM_1D: u32 = 0;
const DIM_2D: u32 = 1;

// The image instructions: sampling (implicit and explicit LOD, with a
// depth reference, projected), fetching, reading, writing, and queries.
const SAMPLE: [u32; 8] = [87, 88, 89, 90, 91, 92, 93, 94];
const PROJ: [u32; 4] = [91, 92, 93, 94];
const DREF: [u32; 4] = [89, 90, 93, 94];
const OP_IMAGE_FETCH: u32 = 95;
const OP_IMAGE_READ: u32 = 98;
const OP_IMAGE_WRITE: u32 = 99;
const OP_IMAGE_QUERY_SIZE_LOD: u32 = 103;
const OP_IMAGE_QUERY_SIZE: u32 = 104;
const OP_IMAGE_QUERY_LOD: u32 = 105;

// Image operands with ids after them, in order: bias, LOD, gradients (two),
// a constant offset, an offset, constant offsets, the sample, a least LOD.
const BIAS: u32 = 0x1;
const LOD: u32 = 0x2;
const GRAD: u32 = 0x4;
const CONST_OFFSET: u32 = 0x8;
const OFFSET: u32 = 0x10;
const CONST_OFFSETS: u32 = 0x20;
const SAMPLE_INDEX: u32 = 0x40;
const MIN_LOD: u32 = 0x80;

struct Made<'a> {
    fresh: &'a mut dyn FnMut() -> u32,
    decls: Vec<Inst>,
    float: u32,
    int: u32,
    vectors: HashMap<(u32, u32), u32>,
    half: u32,
    float_zero: u32,
    int_zero: u32,
    /// Values' components: a vector's (a coordinate may have more than its
    /// image's, which go unused).
    widths: HashMap<u32, u32>,
}

impl Made<'_> {
    fn vector(&mut self, scalar: u32, n: u32) -> u32 {
        if let Some(&v) = self.vectors.get(&(scalar, n)) {
            return v;
        }
        let v = (self.fresh)();
        self.decls.push(inst(OP_TYPE_VECTOR, &[v, scalar, n]));
        self.vectors.insert((scalar, n), v);
        v
    }

    /// A coordinate's x, where it isn't an array's and has more components.
    fn first(&mut self, c: u32, scalar: u32, arrayed: bool, out: &mut Vec<Inst>) -> u32 {
        if arrayed || self.widths.get(&c).is_none_or(|&n| n < 2) {
            return c;
        }
        let x = (self.fresh)();
        out.push(inst(OP_COMPOSITE_EXTRACT, &[scalar, x, c, 0]));
        x
    }

    /// A float coordinate made 2D: x, then `y`, then an array's layer.
    fn float_coordinate(&mut self, c: u32, arrayed: bool, y: u32, out: &mut Vec<Inst>) -> u32 {
        let c = self.first(c, self.float, arrayed, out);
        let v = (self.fresh)();
        if arrayed {
            let (x, l) = ((self.fresh)(), (self.fresh)());
            out.push(inst(OP_COMPOSITE_EXTRACT, &[self.float, x, c, 0]));
            out.push(inst(OP_COMPOSITE_EXTRACT, &[self.float, l, c, 1]));
            let ty = self.vector(self.float, 3);
            out.push(inst(OP_COMPOSITE_CONSTRUCT, &[ty, v, x, y, l]));
        } else {
            let ty = self.vector(self.float, 2);
            out.push(inst(OP_COMPOSITE_CONSTRUCT, &[ty, v, c, y]));
        }
        v
    }

    /// An integer coordinate made 2D: x, then 0, then an array's layer.
    fn int_coordinate(&mut self, c: u32, arrayed: bool, out: &mut Vec<Inst>) -> u32 {
        let c = self.first(c, self.int, arrayed, out);
        let (bits, v) = ((self.fresh)(), (self.fresh)());
        if arrayed {
            let ivec2 = self.vector(self.int, 2);
            out.push(inst(OP_BITCAST, &[ivec2, bits, c]));
            let (x, l) = ((self.fresh)(), (self.fresh)());
            out.push(inst(OP_COMPOSITE_EXTRACT, &[self.int, x, bits, 0]));
            out.push(inst(OP_COMPOSITE_EXTRACT, &[self.int, l, bits, 1]));
            let ty = self.vector(self.int, 3);
            out.push(inst(OP_COMPOSITE_CONSTRUCT, &[ty, v, x, self.int_zero, l]));
        } else {
            out.push(inst(OP_BITCAST, &[self.int, bits, c]));
            let ty = self.vector(self.int, 2);
            out.push(inst(OP_COMPOSITE_CONSTRUCT, &[ty, v, bits, self.int_zero]));
        }
        v
    }

    /// Image operands (from the mask on) with gradients and offsets made 2D.
    fn operands(&mut self, words: &[u32], out: &mut Vec<Inst>) -> Vec<u32> {
        let Some(&mask) = words.first() else { return Vec::new() };
        let mut ids = words[1..].iter().copied();
        let mut result = vec![mask];
        for bit in [BIAS, LOD, GRAD, CONST_OFFSET, OFFSET, CONST_OFFSETS, SAMPLE_INDEX, MIN_LOD] {
            if mask & bit == 0 {
                continue;
            }
            match bit {
                GRAD => {
                    for _ in 0..2 {
                        let d = ids.next().unwrap_or(0);
                        let (ty, v) = (self.vector(self.float, 2), (self.fresh)());
                        out.push(inst(OP_COMPOSITE_CONSTRUCT, &[ty, v, d, self.float_zero]));
                        result.push(v);
                    }
                }
                CONST_OFFSET => {
                    let o = ids.next().unwrap_or(0);
                    let (ty, v) = (self.vector(self.int, 2), (self.fresh)());
                    self.decls.push(inst(OP_CONSTANT_COMPOSITE, &[ty, v, o, self.int_zero]));
                    result.push(v);
                }
                OFFSET => {
                    let o = ids.next().unwrap_or(0);
                    let (bits, v) = ((self.fresh)(), (self.fresh)());
                    out.push(inst(OP_BITCAST, &[self.int, bits, o]));
                    let ty = self.vector(self.int, 2);
                    out.push(inst(OP_COMPOSITE_CONSTRUCT, &[ty, v, bits, self.int_zero]));
                    result.push(v);
                }
                _ => result.extend(ids.next()),
            }
        }
        result.extend(ids);
        result
    }
}

/// The module with its 1D images made 2D (see the module's doc).
pub fn as_2d(insts: Vec<Inst>, fresh: &mut dyn FnMut() -> u32) -> Vec<Inst> {
    // 1D image types (arrayed or not), sampled image types of them, and the
    // values of those types.
    let images: HashMap<u32, bool> = insts.iter().filter(|i| i.op == OP_TYPE_IMAGE && i.words[2] == DIM_1D).map(|i| (i.words[0], i.words[4] == 1)).collect();
    if images.is_empty() {
        return insts;
    }
    let sampled: HashMap<u32, bool> = insts.iter().filter(|i| i.op == OP_TYPE_SAMPLED_IMAGE && images.contains_key(&i.words[1])).map(|i| (i.words[0], images[&i.words[1]])).collect();
    let mut values: HashMap<u32, bool> = HashMap::new();
    let types: HashSet<u32> = insts.iter().filter(|i| (19..=38).contains(&i.op)).map(|i| i.words[0]).collect();
    for i in insts.iter().filter(|i| !(19..=38).contains(&i.op) && i.words.len() >= 2) {
        if let Some(&a) = images.get(&i.words[0]).or_else(|| sampled.get(&i.words[0])) {
            if !types.contains(&i.words[1]) {
                values.insert(i.words[1], a);
            }
        }
    }
    let vector_types: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_TYPE_VECTOR).map(|i| (i.words[0], i.words[2])).collect();
    let widths: HashMap<u32, u32> = insts
        .iter()
        .filter(|i| i.op >= 41 && !(71..=75).contains(&i.op) && i.words.len() >= 2)
        .filter_map(|i| vector_types.get(&i.words[0]).map(|&n| (i.words[1], n)))
        .collect();
    // (A float and an int, vectors of them, and 0.5, 0.0 and 0.)
    let float = insts.iter().find(|i| i.op == OP_TYPE_FLOAT && i.words[1] == 32).map(|i| i.words[0]);
    let mut decls = Vec::new();
    let float = float.unwrap_or_else(|| {
        let f = fresh();
        decls.push(inst(OP_TYPE_FLOAT, &[f, 32]));
        f
    });
    let (int, half, float_zero, int_zero) = (fresh(), fresh(), fresh(), fresh());
    decls.push(inst(OP_TYPE_INT, &[int, 32, 1]));
    decls.push(inst(OP_CONSTANT, &[float, half, 0.5f32.to_bits()]));
    decls.push(inst(OP_CONSTANT, &[float, float_zero, 0]));
    decls.push(inst(OP_CONSTANT, &[int, int_zero, 0]));
    let mut m = Made { fresh, decls, float, int, vectors: HashMap::new(), half, float_zero, int_zero, widths };

    let mut out = Vec::with_capacity(insts.len());
    let mut functions_at = None;
    for i in insts {
        if i.op == OP_FUNCTION && functions_at.is_none() {
            functions_at = Some(out.len());
        }
        let image_of = |k: usize| i.words.get(k).and_then(|v| values.get(v)).copied();
        match i.op {
            OP_TYPE_IMAGE if i.words[2] == DIM_1D => {
                let mut words = i.words.clone();
                words[2] = DIM_2D;
                out.push(Inst { op: i.op, words });
            }
            op if SAMPLE.contains(&op) && image_of(2).is_some() => {
                let arrayed = image_of(2).unwrap();
                let fixed = if DREF.contains(&op) { 5 } else { 4 };
                let coordinate = if PROJ.contains(&op) {
                    // (Projected: (x, q) as (x, q / 2, q), whose division
                    // is (x / q, 0.5); q is the last component.)
                    let (x, q, y, v) = ((m.fresh)(), (m.fresh)(), (m.fresh)(), (m.fresh)());
                    out.push(inst(OP_COMPOSITE_EXTRACT, &[m.float, x, i.words[3], 0]));
                    let last = m.widths.get(&i.words[3]).map_or(1, |n| n - 1);
                    out.push(inst(OP_COMPOSITE_EXTRACT, &[m.float, q, i.words[3], last]));
                    out.push(inst(OP_F_MUL, &[m.float, y, q, m.half]));
                    let ty = m.vector(m.float, 3);
                    out.push(inst(OP_COMPOSITE_CONSTRUCT, &[ty, v, x, y, q]));
                    v
                } else {
                    m.float_coordinate(i.words[3], arrayed, m.half, &mut out)
                };
                let operands = m.operands(&i.words[fixed..], &mut out);
                let mut words = i.words[..fixed].to_vec();
                words[3] = coordinate;
                words.extend(operands);
                out.push(Inst { op, words });
            }
            OP_IMAGE_FETCH | OP_IMAGE_READ if image_of(2).is_some() => {
                let coordinate = m.int_coordinate(i.words[3], image_of(2).unwrap(), &mut out);
                let operands = m.operands(&i.words[4..], &mut out);
                out.push(Inst { op: i.op, words: [&i.words[..3], &[coordinate], &operands[..]].concat() });
            }
            OP_IMAGE_WRITE if image_of(0).is_some() => {
                let coordinate = m.int_coordinate(i.words[1], image_of(0).unwrap(), &mut out);
                let operands = m.operands(&i.words[3..], &mut out);
                out.push(Inst { op: i.op, words: [&[i.words[0], coordinate, i.words[2]], &operands[..]].concat() });
            }
            OP_IMAGE_QUERY_LOD if image_of(2).is_some() => {
                let coordinate = m.float_coordinate(i.words[3], false, m.float_zero, &mut out);
                out.push(inst(i.op, &[i.words[0], i.words[1], i.words[2], coordinate]));
            }
            // (Its size: the width, or the width and the layers.)
            OP_IMAGE_QUERY_SIZE | OP_IMAGE_QUERY_SIZE_LOD if image_of(2).is_some() => {
                let arrayed = image_of(2).unwrap();
                let (ty, result) = (i.words[0], i.words[1]);
                let q = (m.fresh)();
                // (Ints: the result's, as naga takes types alike for one.)
                let wide = m.vector(m.int, if arrayed { 3 } else { 2 });
                out.push(Inst { op: i.op, words: [&[wide, q], &i.words[2..]].concat() });
                if arrayed {
                    out.push(inst(OP_VECTOR_SHUFFLE, &[ty, result, q, q, 0, 2]));
                } else {
                    out.push(inst(OP_COMPOSITE_EXTRACT, &[ty, result, q, 0]));
                }
            }
            _ => out.push(i),
        }
    }
    let at = functions_at.unwrap_or(out.len());
    out.splice(at..at, m.decls);
    out
}
