//! A stage's inputs and outputs that WGSL can't have: arrays, matrices and
//! structs (GLSL's `out float x[4]`, `in mat4 m`, `out Block { ... } b`),
//! which WGSL's entry points take only as scalars and vectors. Such a
//! variable becomes a private one of its type, and each of its scalars or
//! vectors (a matrix's columns) an input or output of its own, at the
//! locations the variable's took one after another (or the members' own),
//! with their interpolation; the entry point copies the inputs into the
//! private variable as it begins and the private variable into the
//! outputs as it returns.

use std::collections::{HashMap, HashSet};

use super::{
    DECORATION_BUILTIN, Inst, OP_ACCESS_CHAIN, OP_CONSTANT, OP_DECORATE, OP_ENTRY_POINT, OP_FUNCTION, OP_IN_BOUNDS_ACCESS_CHAIN, OP_LOAD, OP_MEMBER_DECORATE, OP_STORE,
    OP_TYPE_INT, OP_TYPE_POINTER, OP_VARIABLE, inst,
};

const OP_TYPE_MATRIX: u32 = 24;
const OP_TYPE_ARRAY: u32 = 28;
const OP_TYPE_STRUCT: u32 = 30;
const OP_LABEL: u32 = 248;
const OP_RETURN: u32 = 253;
const STORAGE_INPUT: u32 = 1;
const STORAGE_OUTPUT: u32 = 3;
const STORAGE_PRIVATE: u32 = 6;
const DECORATION_LOCATION: u32 = 30;
/// Interpolation and sampling: NoPerspective, Flat, Centroid, Sample.
const INTERPOLATION: [u32; 4] = [13, 14, 16, 17];

/// One of a variable's scalars or vectors: its indices into the variable,
/// its type, and its own variable.
struct Leaf {
    path: Vec<u32>,
    ty: u32,
    var: u32,
}

/// The module with such inputs and outputs made scalars and vectors (see
/// the module's doc).
pub fn flatten(insts: Vec<Inst>, fresh: &mut dyn FnMut() -> u32) -> Vec<Inst> {
    let types: HashMap<u32, Inst> = insts.iter().filter(|i| (19..=38).contains(&i.op)).map(|i| (i.words[0], i.clone())).collect();
    let constants: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_CONSTANT && i.words.len() == 3).map(|i| (i.words[1], i.words[2])).collect();
    let builtins: HashSet<u32> = insts.iter().filter(|i| i.op == OP_DECORATE && i.words[1] == DECORATION_BUILTIN).map(|i| i.words[0]).collect();
    let builtin_members: HashSet<u32> = insts.iter().filter(|i| i.op == OP_MEMBER_DECORATE && i.words[2] == DECORATION_BUILTIN).map(|i| i.words[0]).collect();
    let mut decorations: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    let mut member_decorations: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::new();
    for i in &insts {
        if i.op == OP_DECORATE {
            decorations.entry(i.words[0]).or_default().push((i.words[1], i.words.get(2).copied().unwrap_or(0)));
        }
        if i.op == OP_MEMBER_DECORATE {
            member_decorations.entry((i.words[0], i.words[1])).or_default().push((i.words[2], i.words.get(3).copied().unwrap_or(0)));
        }
    }
    let pointee = |p: u32| types.get(&p).filter(|t| t.op == OP_TYPE_POINTER).map(|t| (t.words[1], t.words[2]));

    // The variables to flatten: inputs and outputs of arrays, matrices or
    // structs, not builtins.
    let mut flattened: HashMap<u32, (u32, u32)> = HashMap::new();
    for i in insts.iter().filter(|i| i.op == OP_VARIABLE) {
        let (Some((class, ty)), v) = (pointee(i.words[0]), i.words[1]) else { continue; };
        if (class != STORAGE_INPUT && class != STORAGE_OUTPUT) || builtins.contains(&v) || builtin_members.contains(&ty) {
            continue;
        }
        if matches!(types.get(&ty).map(|t| t.op), Some(OP_TYPE_ARRAY | OP_TYPE_MATRIX | OP_TYPE_STRUCT)) {
            flattened.insert(v, (class, ty));
        }
    }
    if flattened.is_empty() {
        return insts;
    }

    let mut made: Vec<Inst> = Vec::new();
    let mut annotations: Vec<Inst> = Vec::new();
    let mut pointers: HashMap<(u32, u32), u32> = insts.iter().filter(|i| i.op == OP_TYPE_POINTER).map(|i| ((i.words[1], i.words[2]), i.words[0])).collect();
    // (The private variables' pointer types, declared just before them.)
    let private_type: HashMap<u32, u32> = flattened.iter().map(|(&v, &(_, ty))| (v, *pointers.entry((STORAGE_PRIVATE, ty)).or_insert_with(&mut *fresh))).collect();
    let declared: HashSet<u32> = insts.iter().filter(|i| i.op == OP_TYPE_POINTER).map(|i| i.words[0]).collect();
    let mut pointer = |made: &mut Vec<Inst>, fresh: &mut dyn FnMut() -> u32, class: u32, ty: u32| -> u32 {
        *pointers.entry((class, ty)).or_insert_with(|| {
            let p = fresh();
            made.push(inst(OP_TYPE_POINTER, &[p, class, ty]));
            p
        })
    };
    // (An int, and constants of it, for the copies' access chains: of
    // their own, declared with the rest.)
    let int = fresh();
    made.push(inst(OP_TYPE_INT, &[int, 32, 1]));
    let mut indices: HashMap<u32, u32> = HashMap::new();
    let mut index = |made: &mut Vec<Inst>, fresh: &mut dyn FnMut() -> u32, v: u32| -> u32 {
        *indices.entry(v).or_insert_with(|| {
            let c = fresh();
            made.push(inst(OP_CONSTANT, &[int, c, v]));
            c
        })
    };

    // Each variable's leaves.
    let mut leaves: HashMap<u32, Vec<Leaf>> = HashMap::new();
    for (&v, &(class, ty)) in &flattened {
        let own = decorations.get(&v).cloned().unwrap_or_default();
        let mut next = own.iter().find(|d| d.0 == DECORATION_LOCATION).map_or(0, |d| d.1);
        let interpolation: Vec<u32> = own.iter().filter(|d| INTERPOLATION.contains(&d.0)).map(|d| d.0).collect();
        let mut out = Vec::new();
        // (Depth first: a member's location, where it has one, sets the
        // count going on from it.)
        fn walk(
            ty: u32,
            path: Vec<u32>,
            interpolation: Vec<u32>,
            next: &mut u32,
            types: &HashMap<u32, Inst>,
            constants: &HashMap<u32, u32>,
            member_decorations: &HashMap<(u32, u32), Vec<(u32, u32)>>,
            out: &mut Vec<(Vec<u32>, u32, u32, Vec<u32>)>,
        ) {
            let t = &types[&ty];
            match t.op {
                OP_TYPE_ARRAY => {
                    for k in 0..constants.get(&t.words[2]).copied().unwrap_or(1) {
                        walk(t.words[1], [&path[..], &[k]].concat(), interpolation.clone(), next, types, constants, member_decorations, out);
                    }
                }
                OP_TYPE_MATRIX => {
                    for k in 0..t.words[2] {
                        walk(t.words[1], [&path[..], &[k]].concat(), interpolation.clone(), next, types, constants, member_decorations, out);
                    }
                }
                OP_TYPE_STRUCT => {
                    for (k, &m) in t.words[1..].iter().enumerate() {
                        let d = member_decorations.get(&(ty, k as u32)).cloned().unwrap_or_default();
                        if let Some(l) = d.iter().find(|d| d.0 == DECORATION_LOCATION) {
                            *next = l.1;
                        }
                        let mut i = interpolation.clone();
                        i.extend(d.iter().filter(|d| INTERPOLATION.contains(&d.0)).map(|d| d.0));
                        walk(m, [&path[..], &[k as u32]].concat(), i, next, types, constants, member_decorations, out);
                    }
                }
                _ => {
                    out.push((path, ty, *next, interpolation));
                    *next += 1;
                }
            }
        }
        let mut found = Vec::new();
        walk(ty, Vec::new(), interpolation, &mut next, &types, &constants, &member_decorations, &mut found);
        for (path, leaf, location, decorations) in found {
            let p = pointer(&mut made, fresh, class, leaf);
            let var = fresh();
            made.push(inst(OP_VARIABLE, &[p, var, class]));
            annotations.push(inst(OP_DECORATE, &[var, DECORATION_LOCATION, location]));
            for &d in &decorations {
                annotations.push(inst(OP_DECORATE, &[var, d]));
            }
            out.push(Leaf { path, ty: leaf, var });
        }
        leaves.insert(v, out);
    }

    // (Pointers into the private variables, access chains, are of the
    // private class.)
    let mut private: HashSet<u32> = flattened.keys().copied().collect();
    let entry_points: Vec<(u32, Vec<u32>)> = insts
        .iter()
        .filter(|i| i.op == OP_ENTRY_POINT)
        .map(|i| {
            let name_end = 2 + i.words[2..].iter().position(|w| w.to_le_bytes().contains(&0)).map_or(i.words.len() - 2, |k| k + 1);
            (i.words[1], i.words[name_end..].to_vec())
        })
        .collect();

    // The copies, as instructions: for an entry point's inputs (at its
    // start) or outputs (at each return).
    let mut copies = |made: &mut Vec<Inst>,
                      fresh: &mut dyn FnMut() -> u32,
                      pointer: &mut dyn FnMut(&mut Vec<Inst>, &mut dyn FnMut() -> u32, u32, u32) -> u32,
                      interface: &[u32],
                      class: u32|
     -> Vec<Inst> {
        let mut out = Vec::new();
        for v in interface.iter().filter(|v| flattened.get(v).is_some_and(|f| f.0 == class)) {
            for leaf in &leaves[v] {
                let p = pointer(made, fresh, STORAGE_PRIVATE, leaf.ty);
                let ids: Vec<u32> = leaf.path.iter().map(|&k| index(made, fresh, k)).collect();
                let (at, value) = (fresh(), fresh());
                out.push(inst(OP_ACCESS_CHAIN, &[&[p, at, *v][..], &ids].concat()));
                if class == STORAGE_INPUT {
                    out.push(inst(OP_LOAD, &[leaf.ty, value, leaf.var]));
                    out.push(inst(OP_STORE, &[at, value]));
                } else {
                    out.push(inst(OP_LOAD, &[leaf.ty, value, at]));
                    out.push(inst(OP_STORE, &[leaf.var, value]));
                }
            }
        }
        out
    };

    let last_annotation = insts.iter().rposition(|i| i.op == OP_DECORATE || i.op == OP_MEMBER_DECORATE).unwrap_or(0);
    let mut out: Vec<Inst> = Vec::with_capacity(insts.len());
    let mut functions_at = None;
    // (Within an entry point: its interface, and whether its first block's
    // variables are still being passed.)
    let mut current: Option<&Vec<u32>> = None;
    let mut starting = false;
    let mut pending = Vec::new();
    let mut pointer_fn = |made: &mut Vec<Inst>, fresh: &mut dyn FnMut() -> u32, class: u32, ty: u32| pointer(made, fresh, class, ty);
    for (n, i) in insts.into_iter().enumerate() {
        if starting && i.op != OP_VARIABLE && i.op != OP_LABEL {
            out.append(&mut pending);
            starting = false;
        }
        match i.op {
            OP_FUNCTION => {
                functions_at.get_or_insert(out.len());
                current = entry_points.iter().find(|e| e.0 == i.words[1]).map(|e| &e.1);
                if let Some(interface) = current {
                    pending = copies(&mut made, fresh, &mut pointer_fn, interface, STORAGE_INPUT);
                    starting = true;
                }
                out.push(i);
            }
            OP_RETURN => {
                if let Some(interface) = current {
                    out.extend(copies(&mut made, fresh, &mut pointer_fn, interface, STORAGE_OUTPUT));
                }
                out.push(i);
            }
            // (The variables private; their locations and interpolation
            // the leaves'.)
            OP_VARIABLE if flattened.contains_key(&i.words[1]) => {
                let (v, p) = (i.words[1], private_type[&i.words[1]]);
                if !declared.contains(&p) {
                    out.push(inst(OP_TYPE_POINTER, &[p, STORAGE_PRIVATE, flattened[&v].1]));
                }
                out.push(inst(OP_VARIABLE, &[p, v, STORAGE_PRIVATE]));
            }
            OP_DECORATE if flattened.contains_key(&i.words[0]) => {}
            OP_ACCESS_CHAIN | OP_IN_BOUNDS_ACCESS_CHAIN if private.contains(&i.words[2]) => {
                let (_, ty) = pointee(i.words[0]).unwrap_or((0, 0));
                let mut words = i.words.clone();
                words[0] = pointer_fn(&mut made, fresh, STORAGE_PRIVATE, ty);
                private.insert(words[1]);
                out.push(Inst { op: i.op, words });
            }
            OP_ENTRY_POINT => {
                let name_end = 2 + i.words[2..].iter().position(|w| w.to_le_bytes().contains(&0)).map_or(i.words.len() - 2, |k| k + 1);
                let mut words = i.words[..name_end].to_vec();
                for w in &i.words[name_end..] {
                    match leaves.get(w) {
                        Some(l) => words.extend(l.iter().map(|l| l.var)),
                        None => words.push(*w),
                    }
                }
                out.push(Inst { op: OP_ENTRY_POINT, words });
            }
            _ => out.push(i),
        }
        if n == last_annotation {
            out.append(&mut annotations);
        }
    }
    let at = functions_at.unwrap_or(out.len());
    out.splice(at..at, made);
    out
}

const OP_TYPE_VECTOR: u32 = 23;
const OP_COMPOSITE_CONSTRUCT: u32 = 80;
const OP_COMPOSITE_EXTRACT: u32 = 81;
const OP_VECTOR_SHUFFLE: u32 = 79;
const OP_CONSTANT_NULL: u32 = 46;
const EXECUTION_FRAGMENT: u32 = 4;

/// A fragment shader's color outputs of fewer than four components made
/// four (WebGPU wants as many as the attachment's format has; Vulkan
/// leaves the rest undefined): stores of them widened with zeros, loads
/// narrowed.
pub fn widen_outputs(insts: Vec<Inst>, fresh: &mut dyn FnMut() -> u32) -> Vec<Inst> {
    let types: HashMap<u32, Inst> = insts.iter().filter(|i| (19..=38).contains(&i.op)).map(|i| (i.words[0], i.clone())).collect();
    let builtins: HashSet<u32> = insts.iter().filter(|i| i.op == OP_DECORATE && i.words[1] == DECORATION_BUILTIN).map(|i| i.words[0]).collect();
    let mut fragment: HashSet<u32> = HashSet::new();
    for i in insts.iter().filter(|i| i.op == OP_ENTRY_POINT && i.words[0] == EXECUTION_FRAGMENT) {
        let name_end = 2 + i.words[2..].iter().position(|w| w.to_le_bytes().contains(&0)).map_or(i.words.len() - 2, |k| k + 1);
        fragment.extend(&i.words[name_end..]);
    }
    // The outputs: variable → (scalar type, components, its type).
    let mut outputs: HashMap<u32, (u32, u32, u32)> = HashMap::new();
    for i in insts.iter().filter(|i| i.op == OP_VARIABLE && i.words[2] == STORAGE_OUTPUT && fragment.contains(&i.words[1]) && !builtins.contains(&i.words[1])) {
        let Some(p) = types.get(&i.words[0]) else { continue; };
        let ty = p.words[2];
        match types.get(&ty).map(|t| (t.op, t.words.clone())) {
            Some((OP_TYPE_VECTOR, w)) if w[2] < 4 => {
                outputs.insert(i.words[1], (w[1], w[2], ty));
            }
            Some((op, _)) if (20..=22).contains(&op) => {
                outputs.insert(i.words[1], (ty, 1, ty));
            }
            _ => {}
        }
    }
    if outputs.is_empty() {
        return insts;
    }
    // Per scalar type: a vector of four, a pointer to it, and a zero,
    // declared before the first output of it (after the scalar type).
    let mut made: HashMap<u32, Vec<Inst>> = HashMap::new();
    let mut wide: HashMap<u32, (u32, u32, u32)> = HashMap::new();
    for &(scalar, _, _) in outputs.values() {
        wide.entry(scalar).or_insert_with(|| {
            let (v, p, z) = (fresh(), fresh(), fresh());
            made.insert(scalar, vec![inst(OP_TYPE_VECTOR, &[v, scalar, 4]), inst(OP_TYPE_POINTER, &[p, STORAGE_OUTPUT, v]), inst(OP_CONSTANT_NULL, &[scalar, z])]);
            (v, p, z)
        });
    }
    let mut out = Vec::with_capacity(insts.len());
    for i in insts {
        if i.op == OP_VARIABLE {
            if let Some(&(scalar, _, _)) = outputs.get(&i.words[1]) {
                out.extend(made.remove(&scalar).unwrap_or_default());
            }
        }
        match i.op {
            OP_VARIABLE if outputs.contains_key(&i.words[1]) => {
                let (_, p, _) = wide[&outputs[&i.words[1]].0];
                out.push(inst(OP_VARIABLE, &[p, i.words[1], STORAGE_OUTPUT]));
            }
            OP_STORE if outputs.contains_key(&i.words[0]) => {
                let (scalar, n, _) = outputs[&i.words[0]];
                let (v, _, zero) = wide[&scalar];
                let mut parts: Vec<u32> = if n == 1 {
                    vec![i.words[1]]
                } else {
                    (0..n)
                        .map(|k| {
                            let x = fresh();
                            out.push(inst(OP_COMPOSITE_EXTRACT, &[scalar, x, i.words[1], k]));
                            x
                        })
                        .collect()
                };
                parts.resize(4, zero);
                let w = fresh();
                out.push(inst(OP_COMPOSITE_CONSTRUCT, &[&[v, w][..], &parts].concat()));
                out.push(inst(OP_STORE, &[i.words[0], w]));
            }
            OP_LOAD if outputs.contains_key(&i.words[2]) => {
                let (scalar, n, _) = outputs[&i.words[2]];
                let (v, _, _) = wide[&scalar];
                let w = fresh();
                out.push(inst(OP_LOAD, &[v, w, i.words[2]]));
                if n == 1 {
                    out.push(inst(OP_COMPOSITE_EXTRACT, &[i.words[0], i.words[1], w, 0]));
                } else {
                    out.push(inst(OP_VECTOR_SHUFFLE, &[&[i.words[0], i.words[1], w, w][..], &(0..n).collect::<Vec<_>>()].concat()));
                }
            }
            // (Access chains into one: to a component, as they were.)
            _ => out.push(i),
        }
    }
    out
}

const BUILTIN_SAMPLE_MASK: u32 = 20;
const OP_BITCAST: u32 = 124;

/// `gl_SampleMask` (an array of one int, in or out), which naga reads as
/// the one u32 WGSL's `sample_mask` is: the variable made a u32, its
/// element's access chains the variable, and what's stored and loaded
/// through them bitcast.
pub fn sample_mask(insts: Vec<Inst>, fresh: &mut dyn FnMut() -> u32) -> Vec<Inst> {
    let masks: HashSet<u32> = insts.iter().filter(|i| i.op == OP_DECORATE && i.words[1..] == [DECORATION_BUILTIN, BUILTIN_SAMPLE_MASK]).map(|i| i.words[0]).collect();
    let types: HashMap<u32, Inst> = insts.iter().filter(|i| (19..=38).contains(&i.op)).map(|i| (i.words[0], i.clone())).collect();
    let arrays: HashSet<u32> = insts
        .iter()
        .filter(|i| i.op == OP_VARIABLE && masks.contains(&i.words[1]))
        .filter(|i| types.get(&i.words[0]).and_then(|p| types.get(&p.words[2])).is_some_and(|t| t.op == OP_TYPE_ARRAY))
        .map(|i| i.words[1])
        .collect();
    if arrays.is_empty() {
        return insts;
    }
    // (A u32, and pointers to it, declared before the first such variable.)
    let uint = fresh();
    let mut pointers: HashMap<u32, u32> = HashMap::new();
    // (Access chains to its element, and the variable they reach.)
    let mut elements: HashMap<u32, u32> = HashMap::new();
    let mut out = Vec::with_capacity(insts.len());
    let mut declared = false;
    for i in insts {
        match i.op {
            OP_VARIABLE if arrays.contains(&i.words[1]) => {
                if !declared {
                    out.push(inst(OP_TYPE_INT, &[uint, 32, 0]));
                    declared = true;
                }
                let class = i.words[2];
                let p = *pointers.entry(class).or_insert_with(&mut *fresh);
                out.push(inst(OP_TYPE_POINTER, &[p, class, uint]));
                out.push(inst(OP_VARIABLE, &[p, i.words[1], class]));
            }
            OP_ACCESS_CHAIN | OP_IN_BOUNDS_ACCESS_CHAIN if arrays.contains(&i.words[2]) && i.words.len() == 4 => {
                elements.insert(i.words[1], i.words[2]);
            }
            OP_STORE if elements.contains_key(&i.words[0]) => {
                let bits = fresh();
                out.push(inst(OP_BITCAST, &[uint, bits, i.words[1]]));
                out.push(inst(OP_STORE, &[elements[&i.words[0]], bits]));
            }
            OP_LOAD if elements.contains_key(&i.words[2]) => {
                let bits = fresh();
                out.push(inst(OP_LOAD, &[uint, bits, elements[&i.words[2]]]));
                out.push(inst(OP_BITCAST, &[i.words[0], i.words[1], bits]));
            }
            _ => out.push(i),
        }
    }
    out
}
