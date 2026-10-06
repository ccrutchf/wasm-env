//! SPIR-V made ready for naga (vulkan.rs): its combined image samplers
//! (GLSL's `sampler2D`), which naga doesn't read, split into an image and a
//! sampler, and its bindings renumbered to leave room for the samplers.
//!
//! Vulkan binding `b` becomes binding `2b`; a combined image sampler's
//! sampler is `2b + 1` (`binding`). A load of a combined image sampler
//! becomes loads of its image and its sampler, put together again by
//! `OpSampledImage`, which naga does read.

use std::collections::HashMap;

use crate::ir::Res;

const OP_ENTRY_POINT: u32 = 15;
const OP_TYPE_SAMPLER: u32 = 26;
const OP_TYPE_SAMPLED_IMAGE: u32 = 27;
const OP_TYPE_POINTER: u32 = 32;
const OP_VARIABLE: u32 = 59;
const OP_LOAD: u32 = 61;
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

/// The module, rewritten (see the module's doc).
pub fn prepare(spirv: &[u8]) -> Res<Vec<u8>> {
    if spirv.len() % 4 != 0 || spirv.len() < 20 {
        return Err("SPIR-V: not a whole number of words".into());
    }
    let words: Vec<u32> = spirv.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    if words[0] != 0x0723_0203 {
        return Err("SPIR-V: no magic number (or not little-endian)".into());
    }
    let mut header = words[..5].to_vec();
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
    let mut next = header[3];
    let mut fresh = || {
        next += 1;
        next - 1
    };

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

    let mut out: Vec<Inst> = Vec::with_capacity(insts.len());
    let mut declared = false;
    for i in insts {
        match i.op {
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
            // uses: the sampler too.)
            OP_ENTRY_POINT => {
                // (Its execution model, its function, its name (a string,
                // through the word with its NUL), then the interface.)
                let name_end = 2 + i.words[2..].iter().position(|w| w.to_le_bytes().contains(&0)).map_or(i.words.len() - 2, |k| k + 1);
                let mut words = i.words.clone();
                words.extend(i.words[name_end..].iter().filter_map(|w| parts.get(w).map(|p| p.1)));
                out.push(Inst { op: OP_ENTRY_POINT, words });
            }
            _ => out.push(i),
        }
    }
    header[3] = next;
    let mut words = header;
    for i in &out {
        words.push(((i.words.len() as u32 + 1) << 16) | i.op);
        words.extend(&i.words);
    }
    Ok(words.iter().flat_map(|w| w.to_le_bytes()).collect())
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
