//! Matrices in buffers laid out as WGSL can't have them: a column stride
//! other than its own (std140's `mat2`, `mat3x2` and `mat4x2`, whose
//! columns are 16 bytes apart), or row-major (`layout(row_major)`), which
//! naga reads only in part. Such a matrix member becomes an array of its
//! columns (or, row-major, of its rows) with the stride as the array's,
//! which WGSL can have; arrays of such matrices, arrays of those arrays.
//!
//! Its uses follow: a load of one is a load of the array, made a matrix
//! (each column extracted, or, row-major, each column put together from
//! the rows' components); a store, the matrix made an array first. Access
//! chains into a column-major one are the same chains (a column is an
//! array element); into a row-major one, a component's chain has its two
//! indices swapped, and a column's (not one place in memory) is a column
//! loaded from the rows, or stored to them, a component at a time.

use std::collections::HashMap;

use super::{
    Inst, OP_ACCESS_CHAIN, OP_CONSTANT, OP_DECORATE, OP_FUNCTION, OP_IN_BOUNDS_ACCESS_CHAIN, OP_LOAD, OP_MEMBER_DECORATE, OP_STORE, OP_TYPE_INT, OP_TYPE_POINTER, OP_TYPE_VECTOR,
    OP_VARIABLE, inst,
};

const OP_TYPE_MATRIX: u32 = 24;
const OP_TYPE_ARRAY: u32 = 28;
const OP_TYPE_RUNTIME_ARRAY: u32 = 29;
const OP_TYPE_STRUCT: u32 = 30;
const OP_FUNCTION_PARAMETER: u32 = 55;
const OP_FUNCTION_CALL: u32 = 57;
const OP_VECTOR_EXTRACT_DYNAMIC: u32 = 77;
const OP_COMPOSITE_CONSTRUCT: u32 = 80;
const OP_COMPOSITE_EXTRACT: u32 = 81;
const OP_COPY_OBJECT: u32 = 83;
const OP_SELECT: u32 = 169;
const OP_PHI: u32 = 245;
const DECORATION_ROW_MAJOR: u32 = 4;
const DECORATION_COL_MAJOR: u32 = 5;
const DECORATION_ARRAY_STRIDE: u32 = 6;
const DECORATION_MATRIX_STRIDE: u32 = 7;

/// A matrix member's layout: row-major or not, and its stride.
type Layout = (bool, u32);

/// Where a pointer points: the type it was declared to (the original), and
/// the layout its matrices have, if it's into a member made arrays.
#[derive(Clone, Copy)]
struct Place {
    ty: u32,
    layout: Option<Layout>,
    class: u32,
}

/// A pointer to a row-major matrix's column, which isn't one place in
/// memory: the pointer to the matrix (its rows' array), and the column's
/// index.
#[derive(Clone, Copy)]
struct Column {
    rows: u32,
    column: u32,
    matrix: u32,
    class: u32,
}

struct Pass<'a> {
    fresh: &'a mut dyn FnMut() -> u32,
    /// Type declarations: by id, the instruction.
    types: HashMap<u32, Inst>,
    constants: HashMap<u32, u32>,
    /// New declarations: types (and constants) before the struct that
    /// needs them; pointer types and constants before the functions;
    /// decorations with the others.
    before: HashMap<u32, Vec<Inst>>,
    late: Vec<Inst>,
    decorations: Vec<Inst>,
    /// Made types: by original and layout; pointer types by class and
    /// pointee; vector types by component and count; index constants.
    made: HashMap<(u32, Layout), u32>,
    pointers: HashMap<(u32, u32), u32>,
    vectors: HashMap<(u32, u32), u32>,
    indices: HashMap<u32, u32>,
    int: Option<u32>,
    /// Array types' strides; the int type, made before every other type.
    strides: HashMap<u32, u32>,
    early: Vec<Inst>,
}

impl Pass<'_> {
    fn ty(&self, id: u32) -> Option<&Inst> {
        self.types.get(&id)
    }

    /// Whether `ty` has a matrix (through arrays).
    fn has_matrix(&self, ty: u32) -> bool {
        match self.ty(ty).map(|i| i.op) {
            Some(OP_TYPE_MATRIX) => true,
            Some(OP_TYPE_ARRAY | OP_TYPE_RUNTIME_ARRAY) => self.has_matrix(self.types[&ty].words[1]),
            _ => false,
        }
    }

    /// A matrix's column type, its columns, and its rows.
    fn matrix(&self, ty: u32) -> (u32, u32, u32) {
        let m = &self.types[&ty];
        let (column, columns) = (m.words[1], m.words[2]);
        (column, columns, self.types[&column].words[2])
    }

    /// The natural column stride of a matrix: WGSL's.
    fn natural(&self, ty: u32) -> u32 {
        let (column, _, rows) = self.matrix(ty);
        let scalar = self.types[&column].words[1];
        let width = self.types.get(&scalar).map_or(4, |s| s.words[1] / 8);
        if rows == 2 { 2 * width } else { 4 * width }
    }

    fn int(&mut self) -> u32 {
        if let Some(t) = self.int {
            return t;
        }
        let t = (self.fresh)();
        self.early.push(inst(OP_TYPE_INT, &[t, 32, 1]));
        self.types.insert(t, inst(OP_TYPE_INT, &[t, 32, 1]));
        self.int = Some(t);
        t
    }

    /// An int constant (for an array's length, or an index).
    fn index(&mut self, v: u32, into: Option<u32>) -> u32 {
        if let Some(&c) = self.indices.get(&v) {
            return c;
        }
        let int = self.int();
        let c = (self.fresh)();
        let i = inst(OP_CONSTANT, &[int, c, v]);
        match into {
            Some(s) => self.before.entry(s).or_default().push(i),
            None => self.late.push(i),
        }
        self.indices.insert(v, c);
        self.constants.insert(c, v);
        c
    }

    fn vector(&mut self, component: u32, n: u32, before: u32) -> u32 {
        if let Some(&v) = self.vectors.get(&(component, n)) {
            return v;
        }
        let v = (self.fresh)();
        self.before.entry(before).or_default().push(inst(OP_TYPE_VECTOR, &[v, component, n]));
        self.types.insert(v, inst(OP_TYPE_VECTOR, &[v, component, n]));
        self.vectors.insert((component, n), v);
        v
    }

    fn pointer(&mut self, class: u32, pointee: u32) -> u32 {
        if let Some(&p) = self.pointers.get(&(class, pointee)) {
            return p;
        }
        let p = (self.fresh)();
        self.late.push(inst(OP_TYPE_POINTER, &[p, class, pointee]));
        self.types.insert(p, inst(OP_TYPE_POINTER, &[p, class, pointee]));
        self.pointers.insert((class, pointee), p);
        p
    }

    /// The type `ty` (a matrix, or arrays of them) is made in a member of
    /// `layout`, declared before struct `before`.
    fn made(&mut self, ty: u32, layout: Layout, before: u32) -> u32 {
        if let Some(&t) = self.made.get(&(ty, layout)) {
            return t;
        }
        let i = self.types[&ty].clone();
        let t = (self.fresh)();
        match i.op {
            OP_TYPE_MATRIX => {
                let (column, columns, rows) = self.matrix(ty);
                let (element, n) = if layout.0 { (self.vector(self.types[&column].words[1], columns, before), rows) } else { (column, columns) };
                let n = self.index(n, Some(before));
                self.before.entry(before).or_default().push(inst(OP_TYPE_ARRAY, &[t, element, n]));
                self.types.insert(t, inst(OP_TYPE_ARRAY, &[t, element, n]));
                self.decorations.push(inst(OP_DECORATE, &[t, DECORATION_ARRAY_STRIDE, layout.1]));
            }
            _ => {
                let element = self.made(i.words[1], layout, before);
                let mut words = i.words.clone();
                words[0] = t;
                words[1] = element;
                self.types.insert(t, Inst { op: i.op, words: words.clone() });
                self.before.entry(before).or_default().push(Inst { op: i.op, words });
                if let Some(s) = self.stride(ty) {
                    self.decorations.push(inst(OP_DECORATE, &[t, DECORATION_ARRAY_STRIDE, s]));
                }
            }
        }
        self.made.insert((ty, layout), t);
        t
    }

    fn stride(&self, ty: u32) -> Option<u32> {
        self.strides.get(&ty).copied()
    }
}

/// The module with such matrices made arrays (see the module's doc).
pub fn arrays(insts: Vec<Inst>, fresh: &mut dyn FnMut() -> u32) -> Vec<Inst> {
    // Member layouts: (struct, member) → row-major, stride.
    let mut row_major: HashMap<(u32, u32), bool> = HashMap::new();
    let mut strides: HashMap<(u32, u32), u32> = HashMap::new();
    for i in &insts {
        if i.op == OP_MEMBER_DECORATE {
            match i.words[2] {
                DECORATION_ROW_MAJOR => {
                    row_major.insert((i.words[0], i.words[1]), true);
                }
                DECORATION_MATRIX_STRIDE => {
                    strides.insert((i.words[0], i.words[1]), i.words[3]);
                }
                _ => {}
            }
        }
    }
    if strides.is_empty() && row_major.is_empty() {
        return insts;
    }
    let types: HashMap<u32, Inst> = insts.iter().filter(|i| (19..=38).contains(&i.op)).map(|i| (i.words[0], i.clone())).collect();
    let mut pass = Pass {
        fresh,
        constants: insts.iter().filter(|i| i.op == OP_CONSTANT && i.words.len() == 3).map(|i| (i.words[1], i.words[2])).collect(),
        types,
        before: HashMap::new(),
        late: Vec::new(),
        decorations: Vec::new(),
        made: HashMap::new(),
        pointers: insts.iter().filter(|i| i.op == OP_TYPE_POINTER).map(|i| ((i.words[1], i.words[2]), i.words[0])).collect(),
        // (Vectors of its own too: the module's may come after a struct
        // that needs one.)
        vectors: HashMap::new(),
        indices: HashMap::new(),
        // (An int of its own, declared before every type: the module's
        // may come after a struct that needs it.)
        int: None,
        strides: insts.iter().filter(|i| i.op == OP_DECORATE && i.words[1] == DECORATION_ARRAY_STRIDE).map(|i| (i.words[0], i.words[2])).collect(),
        early: Vec::new(),
    };

    // The members made arrays: their layouts, and their original types (in
    // the order they're declared, as what's made for them is).
    let mut order = Vec::new();
    let mut members: HashMap<(u32, u32), (Layout, u32)> = HashMap::new();
    for i in insts.iter().filter(|i| i.op == OP_TYPE_STRUCT) {
        for (k, &ty) in i.words[1..].iter().enumerate() {
            let key = (i.words[0], k as u32);
            if !pass.has_matrix(ty) {
                continue;
            }
            let mut m = ty;
            while pass.types[&m].op != OP_TYPE_MATRIX {
                m = pass.types[&m].words[1];
            }
            let rm = row_major.get(&key).copied().unwrap_or(false);
            let stride = strides.get(&key).copied().unwrap_or_else(|| pass.natural(m));
            if rm || stride != pass.natural(m) {
                members.insert(key, ((rm, stride), ty));
                order.push(key);
            }
        }
    }
    if members.is_empty() {
        return insts;
    }
    for &(s, k) in &order {
        let (layout, ty) = members[&(s, k)];
        pass.made(ty, layout, s);
    }
    // (Index constants loads and stores of row-major columns take.)
    let most = members.values().map(|&(_, ty)| {
        let mut m = ty;
        while pass.types[&m].op != OP_TYPE_MATRIX {
            m = pass.types[&m].words[1];
        }
        let (_, c, r) = pass.matrix(m);
        c.max(r)
    });
    for v in 0..most.max().unwrap_or(0) {
        pass.index(v, None);
    }

    let last_decoration = insts.iter().rposition(|i| i.op == OP_DECORATE || i.op == OP_MEMBER_DECORATE).unwrap_or(0);
    let mut places: HashMap<u32, Place> = HashMap::new();
    let mut columns: HashMap<u32, Column> = HashMap::new();
    let mut values: HashMap<u32, u32> = HashMap::new();
    let mut out: Vec<Inst> = Vec::with_capacity(insts.len());
    // (Where the functions begin: what they need is declared there, at
    // the end.)
    let mut functions = None;
    for (n, i) in insts.into_iter().enumerate() {
        if (19..=38).contains(&i.op) {
            out.append(&mut pass.early);
        }
        if i.op == OP_FUNCTION && functions.is_none() {
            functions = Some(out.len());
        }
        match i.op {
            // (A made member's majority and stride go: it's an array.)
            OP_MEMBER_DECORATE
                if members.contains_key(&(i.words[0], i.words[1])) && matches!(i.words[2], DECORATION_ROW_MAJOR | DECORATION_COL_MAJOR | DECORATION_MATRIX_STRIDE) => {}
            OP_TYPE_STRUCT => {
                out.extend(pass.before.remove(&i.words[0]).unwrap_or_default());
                let mut words = i.words.clone();
                for k in 1..words.len() {
                    if let Some(&(layout, ty)) = members.get(&(i.words[0], k as u32 - 1)) {
                        words[k] = pass.made[&(ty, layout)];
                    }
                }
                out.push(Inst { op: OP_TYPE_STRUCT, words });
            }
            OP_VARIABLE => {
                if let Some(p) = pass.ty(i.words[0]) {
                    places.insert(i.words[1], Place { ty: p.words[2], layout: None, class: p.words[1] });
                }
                out.push(i);
            }
            OP_FUNCTION_PARAMETER => {
                if let Some(p) = pass.ty(i.words[0]).filter(|p| p.op == OP_TYPE_POINTER) {
                    places.insert(i.words[1], Place { ty: p.words[2], layout: None, class: p.words[1] });
                }
                values.insert(i.words[1], i.words[0]);
                out.push(i);
            }
            OP_ACCESS_CHAIN | OP_IN_BOUNDS_ACCESS_CHAIN => chain(&mut pass, &members, &mut places, &mut columns, i, &mut out),
            OP_LOAD if columns.contains_key(&i.words[2]) => {
                let c = columns[&i.words[2]];
                let (ty, result) = (i.words[0], i.words[1]);
                let rows = (pass.fresh)();
                out.push(inst(OP_LOAD, &[c.rows, rows, c.matrix]));
                let row = pass.types[&c.rows].words[1];
                let scalar = pass.types[&row].words[1];
                let n = pass.types[&ty].words[2];
                let mut parts = Vec::new();
                for r in 0..n {
                    let (v, x) = ((pass.fresh)(), (pass.fresh)());
                    out.push(inst(OP_COMPOSITE_EXTRACT, &[row, v, rows, r]));
                    out.push(inst(OP_VECTOR_EXTRACT_DYNAMIC, &[scalar, x, v, c.column]));
                    parts.push(x);
                }
                out.push(inst(OP_COMPOSITE_CONSTRUCT, &[&[ty, result][..], &parts].concat()));
                values.insert(result, ty);
            }
            OP_LOAD if places.get(&i.words[2]).is_some_and(|p| p.layout.is_some() && pass.has_matrix(p.ty)) => {
                let p = places[&i.words[2]];
                let (ty, result) = (i.words[0], i.words[1]);
                let made = pass.made[&(p.ty, p.layout.unwrap())];
                let loaded = (pass.fresh)();
                let mut words = i.words.clone();
                words[0] = made;
                words[1] = loaded;
                out.push(Inst { op: OP_LOAD, words });
                to_matrix(&mut pass, p.ty, p.layout.unwrap(), loaded, Some(result), &mut out);
                values.insert(result, ty);
            }
            OP_STORE if columns.contains_key(&i.words[0]) => {
                let c = columns[&i.words[0]];
                let row = pass.types[&c.rows].words[1];
                let scalar = pass.types[&row].words[1];
                let n = pass.types[&c.rows].words[2];
                let n = pass.constants.get(&n).copied().unwrap_or(0);
                let to = pass.pointer(c.class, scalar);
                for r in 0..n {
                    let (x, p) = ((pass.fresh)(), (pass.fresh)());
                    out.push(inst(OP_COMPOSITE_EXTRACT, &[scalar, x, i.words[1], r]));
                    let index = pass.index(r, None);
                    out.push(inst(OP_ACCESS_CHAIN, &[to, p, c.matrix, index, c.column]));
                    out.push(inst(OP_STORE, &[p, x]));
                }
            }
            OP_STORE if places.get(&i.words[0]).is_some_and(|p| p.layout.is_some() && pass.has_matrix(p.ty)) => {
                let p = places[&i.words[0]];
                let value = from_matrix(&mut pass, p.ty, p.layout.unwrap(), i.words[1], &mut out);
                let mut words = i.words.clone();
                words[1] = value;
                out.push(Inst { op: OP_STORE, words });
            }
            OP_COMPOSITE_EXTRACT => {
                let (ty, result, composite) = (i.words[0], i.words[1], i.words[2]);
                values.insert(result, ty);
                // (Through a made member: extracted to there as the array,
                // made a matrix, and the rest from that.)
                let mut t = values.get(&composite).copied();
                let mut split = None;
                for (k, &index) in i.words[3..].iter().enumerate() {
                    let Some(s) = t.and_then(|t| pass.ty(t)).cloned() else { break; };
                    t = match s.op {
                        OP_TYPE_STRUCT => {
                            if let Some(&(layout, member)) = members.get(&(s.words[0], index)) {
                                split = Some((k, layout, member));
                                break;
                            }
                            s.words.get(1 + index as usize).copied()
                        }
                        OP_TYPE_ARRAY | OP_TYPE_RUNTIME_ARRAY | OP_TYPE_VECTOR | OP_TYPE_MATRIX => Some(s.words[1]),
                        _ => None,
                    };
                }
                match split {
                    Some((k, layout, member)) => {
                        let array = (pass.fresh)();
                        out.push(inst(OP_COMPOSITE_EXTRACT, &[&[pass.made[&(member, layout)], array, composite][..], &i.words[3..4 + k]].concat()));
                        if i.words.len() == 4 + k {
                            to_matrix(&mut pass, member, layout, array, Some(result), &mut out);
                        } else {
                            let m = to_matrix(&mut pass, member, layout, array, None, &mut out);
                            out.push(inst(OP_COMPOSITE_EXTRACT, &[&[ty, result, m][..], &i.words[4 + k..]].concat()));
                        }
                    }
                    None => out.push(i),
                }
            }
            OP_LOAD | OP_COMPOSITE_CONSTRUCT | OP_COPY_OBJECT | OP_FUNCTION_CALL | OP_PHI | OP_SELECT => {
                values.insert(i.words[1], i.words[0]);
                out.push(i);
            }
            _ => out.push(i),
        }
        if n == last_decoration {
            out.append(&mut pass.decorations);
        }
    }
    let at = functions.unwrap_or(out.len());
    out.splice(at..at, pass.late);
    out
}

/// An access chain, through a made member: its result type the made one,
/// or into a row-major matrix, split there (see the module's doc).
fn chain(pass: &mut Pass, members: &HashMap<(u32, u32), (Layout, u32)>, places: &mut HashMap<u32, Place>, columns: &mut HashMap<u32, Column>, i: Inst, out: &mut Vec<Inst>) {
    let (ty, result, base) = (i.words[0], i.words[1], i.words[2]);
    let indices = &i.words[3..];
    // (A row-major column's component: the rows' chain, swapped.)
    if let Some(&c) = columns.get(&base) {
        out.push(inst(i.op, &[&[ty, result, c.matrix][..], &indices[..1], &[c.column], &indices[1..]].concat()));
        return;
    }
    let Some(&start) = places.get(&base) else {
        out.push(i);
        return;
    };
    let mut place = start;
    for (k, &index) in indices.iter().enumerate() {
        let Some(s) = pass.ty(place.ty).cloned() else { break; };
        match s.op {
            OP_TYPE_STRUCT => {
                let k = pass.constants.get(&index).copied().unwrap_or(0);
                place.ty = s.words[1 + k as usize];
                place.layout = members.get(&(s.words[0], k)).map(|&(l, _)| l);
            }
            OP_TYPE_ARRAY | OP_TYPE_RUNTIME_ARRAY | OP_TYPE_VECTOR => place.ty = s.words[1],
            OP_TYPE_MATRIX => match place.layout {
                Some((true, _)) => {
                    // The chain to the matrix (its rows), then the column.
                    let rows = pass.made[&(place.ty, place.layout.unwrap())];
                    let matrix = if k == 0 {
                        base
                    } else {
                        let m = (pass.fresh)();
                        let to = pass.pointer(place.class, rows);
                        out.push(inst(i.op, &[&[to, m, base][..], &indices[..k]].concat()));
                        m
                    };
                    match &indices[k..] {
                        [column] => {
                            columns.insert(result, Column { rows, column: *column, matrix, class: place.class });
                        }
                        [column, row, ..] => out.push(inst(i.op, &[ty, result, matrix, *row, *column])),
                        [] => unreachable!(),
                    }
                    return;
                }
                _ => {
                    place.ty = s.words[1];
                    place.layout = None;
                }
            },
            _ => break,
        }
    }
    let mut words = i.words.clone();
    if let Some(layout) = place.layout.filter(|_| pass.has_matrix(place.ty)) {
        let made = pass.made[&(place.ty, layout)];
        words[0] = pass.pointer(place.class, made);
    }
    places.insert(result, place);
    out.push(Inst { op: i.op, words });
}

/// `value`, of type `ty` made with `layout`, as `ty` (into `result`, if
/// given): the id it's in.
fn to_matrix(pass: &mut Pass, ty: u32, layout: Layout, value: u32, result: Option<u32>, out: &mut Vec<Inst>) -> u32 {
    let result = result.unwrap_or_else(|| (pass.fresh)());
    let t = pass.types[&ty].clone();
    let mut parts = Vec::new();
    if t.op == OP_TYPE_MATRIX {
        let (column, columns, rows) = pass.matrix(ty);
        if layout.0 {
            let row = pass.types[&pass.made[&(ty, layout)]].words[1];
            let scalar = pass.types[&column].words[1];
            let rs: Vec<u32> = (0..rows)
                .map(|r| {
                    let v = (pass.fresh)();
                    out.push(inst(OP_COMPOSITE_EXTRACT, &[row, v, value, r]));
                    v
                })
                .collect();
            for c in 0..columns {
                let xs: Vec<u32> = rs
                    .iter()
                    .map(|&r| {
                        let x = (pass.fresh)();
                        out.push(inst(OP_COMPOSITE_EXTRACT, &[scalar, x, r, c]));
                        x
                    })
                    .collect();
                let v = (pass.fresh)();
                out.push(inst(OP_COMPOSITE_CONSTRUCT, &[&[column, v][..], &xs].concat()));
                parts.push(v);
            }
        } else {
            for c in 0..columns {
                let v = (pass.fresh)();
                out.push(inst(OP_COMPOSITE_EXTRACT, &[column, v, value, c]));
                parts.push(v);
            }
        }
    } else {
        let n = pass.constants.get(&t.words[2]).copied().unwrap_or(0);
        let element = pass.made[&(t.words[1], layout)];
        for k in 0..n {
            let e = (pass.fresh)();
            out.push(inst(OP_COMPOSITE_EXTRACT, &[element, e, value, k]));
            parts.push(to_matrix(pass, t.words[1], layout, e, None, out));
        }
    }
    out.push(inst(OP_COMPOSITE_CONSTRUCT, &[&[ty, result][..], &parts].concat()));
    result
}

/// `value`, of type `ty`, as `ty` made with `layout`: the id it's in.
fn from_matrix(pass: &mut Pass, ty: u32, layout: Layout, value: u32, out: &mut Vec<Inst>) -> u32 {
    let made = pass.made[&(ty, layout)];
    let t = pass.types[&ty].clone();
    let mut parts = Vec::new();
    if t.op == OP_TYPE_MATRIX {
        let (column, columns, rows) = pass.matrix(ty);
        if layout.0 {
            let row = pass.types[&made].words[1];
            let scalar = pass.types[&column].words[1];
            for r in 0..rows {
                let xs: Vec<u32> = (0..columns)
                    .map(|c| {
                        let x = (pass.fresh)();
                        out.push(inst(OP_COMPOSITE_EXTRACT, &[scalar, x, value, c, r]));
                        x
                    })
                    .collect();
                let v = (pass.fresh)();
                out.push(inst(OP_COMPOSITE_CONSTRUCT, &[&[row, v][..], &xs].concat()));
                parts.push(v);
            }
        } else {
            for c in 0..columns {
                let v = (pass.fresh)();
                out.push(inst(OP_COMPOSITE_EXTRACT, &[column, v, value, c]));
                parts.push(v);
            }
        }
    } else {
        let n = pass.constants.get(&t.words[2]).copied().unwrap_or(0);
        for k in 0..n {
            let e = (pass.fresh)();
            out.push(inst(OP_COMPOSITE_EXTRACT, &[t.words[1], e, value, k]));
            parts.push(from_matrix(pass, t.words[1], layout, e, out));
        }
    }
    let result = (pass.fresh)();
    out.push(inst(OP_COMPOSITE_CONSTRUCT, &[&[made, result][..], &parts].concat()));
    result
}
