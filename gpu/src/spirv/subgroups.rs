//! Subgroups of one invocation, on any device (the driver reports a
//! subgroup size of 1, with basic and vote operations): what they are made
//! of in the SPIR-V. `gl_SubgroupSize` is 1, `gl_SubgroupInvocationID` 0,
//! `gl_NumSubgroups` the work group's size and `gl_SubgroupID` the
//! invocation's index in it; `subgroupElect()` is true, `subgroupAll(x)`
//! and `subgroupAny(x)` are x, `subgroupAllEqual` true; barriers within a
//! subgroup go. (A device's own subgroups would be a fast path.) And
//! `gl_HelperInvocation`, which WGSL hasn't, is false: a helper's results
//! go nowhere.

use std::collections::HashMap;

use super::{DECORATION_BUILTIN, Inst, OP_CONSTANT, OP_DECORATE, OP_ENTRY_POINT, OP_FUNCTION, OP_LOAD, OP_TYPE_INT, OP_TYPE_POINTER, OP_VARIABLE, inst};

const OP_EXECUTION_MODE: u32 = 16;
const OP_CONSTANT_TRUE: u32 = 41;
const OP_COPY_OBJECT: u32 = 83;
const OP_CONTROL_BARRIER: u32 = 224;
const OP_MEMORY_BARRIER: u32 = 225;
const OP_GROUP_NON_UNIFORM_ELECT: u32 = 333;
const OP_GROUP_NON_UNIFORM_ALL: u32 = 334;
const OP_GROUP_NON_UNIFORM_ANY: u32 = 335;
const OP_GROUP_NON_UNIFORM_ALL_EQUAL: u32 = 336;
const MODE_LOCAL_SIZE: u32 = 17;
const SCOPE_SUBGROUP: u32 = 3;
const STORAGE_INPUT: u32 = 1;
const OP_CONSTANT_FALSE: u32 = 42;
const BUILTIN_HELPER_INVOCATION: u32 = 23;
const BUILTIN_LOCAL_INVOCATION_INDEX: u32 = 29;
const BUILTIN_SUBGROUP_SIZE: u32 = 36;
const BUILTIN_NUM_SUBGROUPS: u32 = 38;
const BUILTIN_SUBGROUP_ID: u32 = 40;
const BUILTIN_SUBGROUP_LOCAL_INVOCATION_ID: u32 = 41;

/// The module with its subgroups made of one invocation (see the module's
/// doc).
pub fn single(insts: Vec<Inst>, fresh: &mut dyn FnMut() -> u32) -> Vec<Inst> {
    let builtin: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_DECORATE && i.words[1] == DECORATION_BUILTIN).map(|i| (i.words[0], i.words[2])).collect();
    let subgroup_vars: HashMap<u32, u32> =
        builtin.iter().filter(|(_, b)| matches!(**b, BUILTIN_SUBGROUP_SIZE | BUILTIN_NUM_SUBGROUPS | BUILTIN_SUBGROUP_ID | BUILTIN_SUBGROUP_LOCAL_INVOCATION_ID | BUILTIN_HELPER_INVOCATION)).map(|(&v, &b)| (v, b)).collect();
    let uses_ops = insts.iter().any(|i| (OP_GROUP_NON_UNIFORM_ELECT..=OP_GROUP_NON_UNIFORM_ALL_EQUAL).contains(&i.op));
    let constants: HashMap<u32, u32> = insts.iter().filter(|i| i.op == OP_CONSTANT && i.words.len() == 3).map(|i| (i.words[1], i.words[2])).collect();
    let subgroup_scope = |id: u32| constants.get(&id) == Some(&SCOPE_SUBGROUP);
    let barriers = insts.iter().any(|i| (i.op == OP_CONTROL_BARRIER || i.op == OP_MEMORY_BARRIER) && subgroup_scope(i.words[0]));
    if subgroup_vars.is_empty() && !uses_ops && !barriers {
        return insts;
    }
    // The work group's size (1 outside compute shaders).
    let size: u32 = insts.iter().find(|i| i.op == OP_EXECUTION_MODE && i.words[1] == MODE_LOCAL_SIZE).map_or(1, |i| i.words[2..5].iter().product());
    // What the loads become: constants (made before the functions), or, for
    // `gl_SubgroupID`, a load of the local invocation index (a variable
    // made if there's none, added to the entry points' interfaces).
    let mut made = Vec::new();
    let mut values: HashMap<(u32, u32), u32> = HashMap::new();
    let index_var = builtin.iter().find(|(_, b)| **b == BUILTIN_LOCAL_INVOCATION_INDEX).map(|(&v, _)| v);
    let wants_index = subgroup_vars.values().any(|&b| b == BUILTIN_SUBGROUP_ID) && size > 1;
    let mut new_index = None;
    let mut index_annotations = Vec::new();
    let index_var = match (index_var, wants_index) {
        (Some(v), _) => Some(v),
        (None, true) => {
            let (uint, p, v) = (fresh(), fresh(), fresh());
            made.push(inst(OP_TYPE_INT, &[uint, 32, 0]));
            made.push(inst(OP_TYPE_POINTER, &[p, STORAGE_INPUT, uint]));
            made.push(inst(OP_VARIABLE, &[p, v, STORAGE_INPUT]));
            index_annotations.push(inst(OP_DECORATE, &[v, DECORATION_BUILTIN, BUILTIN_LOCAL_INVOCATION_INDEX]));
            new_index = Some(v);
            Some(v)
        }
        (None, false) => None,
    };
    let mut trues: HashMap<u32, u32> = HashMap::new();

    let last_annotation = insts.iter().rposition(|i| i.op == OP_DECORATE).unwrap_or(0);
    let mut out = Vec::with_capacity(insts.len());
    let mut functions_at = None;
    for (n, i) in insts.into_iter().enumerate() {
        match i.op {
            OP_FUNCTION => {
                functions_at.get_or_insert(out.len());
                out.push(i);
            }
            OP_VARIABLE if subgroup_vars.contains_key(&i.words[1]) => {}
            OP_DECORATE if subgroup_vars.contains_key(&i.words[0]) => {}
            OP_ENTRY_POINT => {
                let name_end = 2 + i.words[2..].iter().position(|w| w.to_le_bytes().contains(&0)).map_or(i.words.len() - 2, |k| k + 1);
                let mut words: Vec<u32> = i.words[..name_end].iter().chain(i.words[name_end..].iter().filter(|w| !subgroup_vars.contains_key(w))).copied().collect();
                words.extend(new_index);
                out.push(Inst { op: OP_ENTRY_POINT, words });
            }
            OP_LOAD if subgroup_vars.contains_key(&i.words[2]) => {
                let (ty, result) = (i.words[0], i.words[1]);
                match subgroup_vars[&i.words[2]] {
                    BUILTIN_SUBGROUP_ID if size > 1 => out.push(inst(OP_LOAD, &[ty, result, index_var.unwrap()])),
                    BUILTIN_HELPER_INVOCATION => {
                        let f = fresh();
                        made.push(inst(OP_CONSTANT_FALSE, &[ty, f]));
                        out.push(inst(OP_COPY_OBJECT, &[ty, result, f]));
                    }
                    b => {
                        let v = match b {
                            BUILTIN_SUBGROUP_SIZE => 1,
                            BUILTIN_NUM_SUBGROUPS => size,
                            _ => 0,
                        };
                        let c = *values.entry((ty, v)).or_insert_with(|| {
                            let c = fresh();
                            made.push(inst(OP_CONSTANT, &[ty, c, v]));
                            c
                        });
                        out.push(inst(OP_COPY_OBJECT, &[ty, result, c]));
                    }
                }
            }
            OP_GROUP_NON_UNIFORM_ELECT | OP_GROUP_NON_UNIFORM_ALL_EQUAL => {
                let (ty, result) = (i.words[0], i.words[1]);
                let t = *trues.entry(ty).or_insert_with(|| {
                    let c = fresh();
                    made.push(inst(OP_CONSTANT_TRUE, &[ty, c]));
                    c
                });
                out.push(inst(OP_COPY_OBJECT, &[ty, result, t]));
            }
            OP_GROUP_NON_UNIFORM_ALL | OP_GROUP_NON_UNIFORM_ANY => out.push(inst(OP_COPY_OBJECT, &[i.words[0], i.words[1], i.words[3]])),
            OP_CONTROL_BARRIER | OP_MEMORY_BARRIER if subgroup_scope(i.words[0]) => {}
            _ => out.push(i),
        }
        if n == last_annotation {
            out.append(&mut index_annotations);
        }
    }
    let at = functions_at.unwrap_or(out.len());
    out.splice(at..at, made);
    out
}
