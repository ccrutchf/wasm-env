//! An OpenCL kernel's LLVM IR as a WGSL compute shader.
//!
//! **Values.** Every SSA value is a function variable, declared up front
//! (so uses in other structured scopes see it); phis are assigned on the
//! edges into their block. `i1` is `bool`; `i8`, `i16` and `i32` are `u32`,
//! narrow ones kept zero-extended (signed operations sign-extend them as
//! they go); `float` is `f32`; vectors of up to four are WGSL vectors.
//! `i64`, `half` and `double` aren't supported yet (the device reports no
//! fp64 or fp16).
//!
//! **Memory.** A pointer is a byte offset (`u32`: the spir target's
//! pointers are 32-bit) into a base the compiler knows from where the
//! pointer came from: a buffer argument (a storage binding), local memory
//! (a workgroup array), a private array (an `alloca`), a program-scope
//! constant, or the argument block. Each base is an array of `u32` words,
//! read and written whole where alignment allows and by bytes otherwise;
//! it's made of atomics where the kernel uses atomics on it, or stores
//! less than a word to it (so neighbouring bytes other work-items write
//! aren't lost). A pointer whose base can't be known (a phi of two
//! buffers, a pointer loaded from memory) is reported.
//!
//! **Arguments.** Binding 0 is the argument block (`array<u32>`): a header
//! (below), then each argument's bytes at its offset (a buffer argument's
//! byte offset in its buffer; values as OpenCL lays them out). Buffer
//! argument k is binding k + 1.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

use crate::ir::{Const, Function, Inst, Module, Op, Operand, Res, Type, Value, bail};
use crate::structure::{Node, structure};

/// The argument block's header, in words: the global offset (3), the
/// global size (3), the work dimension, and the group offset (3) of this
/// dispatch (a launch larger than one dispatch allows is several).
pub const HEADER_WORDS: u32 = 16;
const GLOBAL_OFFSET: u32 = 0;
const GLOBAL_SIZE: u32 = 3;
const WORK_DIM: u32 = 6;
const GROUP_OFFSET: u32 = 7;

#[derive(Clone, Debug, PartialEq)]
pub enum ArgKind {
    /// A `__global` (or `__constant`) buffer.
    Buffer { constant: bool },
    /// `__local` memory of the size the host gives.
    Local,
    /// A value (scalar, vector or struct) of `size` bytes.
    Value { size: u32 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub kind: ArgKind,
    /// Byte offset in the argument block.
    pub offset: u32,
    /// OpenCL's description, for clGetKernelArgInfo.
    pub type_name: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KernelInfo {
    pub name: String,
    pub args: Vec<Arg>,
    /// The argument block's size in bytes.
    pub block_size: u32,
    /// `reqd_work_group_size`, if the kernel has one.
    pub required_local: Option<[u32; 3]>,
}

/// A kernel's arguments and their layout.
pub fn kernel_info(m: &Module, name: &str) -> Res<KernelInfo> {
    let f = m.function(name).filter(|f| f.kernel).ok_or_else(|| format!("no kernel {name}"))?;
    let types = f.metadata.get("kernel_arg_type").map(|&n| m.meta_strings(n)).unwrap_or_default();
    let names = f.metadata.get("kernel_arg_name").map(|&n| m.meta_strings(n)).unwrap_or_default();
    let mut offset = HEADER_WORDS * 4;
    let mut args = Vec::new();
    for (k, t) in f.params.iter().enumerate() {
        let (kind, size, align) = match t {
            Type::Ptr(1) => (ArgKind::Buffer { constant: false }, 4, 4),
            Type::Ptr(2) => (ArgKind::Buffer { constant: true }, 4, 4),
            Type::Ptr(3) => (ArgKind::Local, 0, 1),
            // A struct by value arrives as a pointer to it (byval): its
            // bytes are in the block.
            Type::Ptr(0) => {
                let Some(t) = byval_type(m, f, k) else { bail!("argument {k} is a private pointer") };
                let (s, a) = m.layout(&t)?;
                (ArgKind::Value { size: s as u32 }, s as u32, a as u32)
            }
            t => {
                let (s, a) = m.layout(t)?;
                (ArgKind::Value { size: s as u32 }, s as u32, a as u32)
            }
        };
        offset = offset.next_multiple_of(align.max(1));
        args.push(Arg {
            kind,
            offset,
            type_name: types.get(k).cloned().unwrap_or_default(),
            name: names.get(k).cloned().unwrap_or_default(),
        });
        offset += size;
    }
    let required_local = f.metadata.get("reqd_work_group_size").map(|&n| m.meta_ints(n)).filter(|v| v.len() == 3).map(|v| [v[0] as u32, v[1] as u32, v[2] as u32]);
    Ok(KernelInfo { name: name.to_string(), args, block_size: offset.next_multiple_of(4), required_local })
}

/// A byval argument's struct type: recorded by the parser from the
/// signature's `byval(T)`.
fn byval_type(_m: &Module, f: &Function, k: usize) -> Option<Type> {
    f.byval.get(&k).cloned()
}

/// What a launch fixes that the shader depends on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Launch {
    pub local: [u32; 3],
    /// Bytes of `__local` memory for each local-pointer argument.
    pub local_bytes: Vec<(usize, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Base {
    /// The argument block.
    Block,
    /// Buffer argument k.
    Buffer(usize),
    /// Local-memory argument k.
    LocalArg(usize),
    /// An alloca (by its value number).
    Private(usize),
    /// A program-scope variable (by index in the module's globals).
    Global(usize),
}

pub fn compile(m: &Module, name: &str, launch: &Launch) -> Res<String> {
    let f = m.function(name).filter(|f| f.kernel).ok_or_else(|| format!("no kernel {name}"))?;
    let info = kernel_info(m, name)?;
    for b in &f.blocks {
        for i in &b.insts {
            if let Op::Call(callee, _) = &i.op
                && m.function(callee).is_some()
            {
                bail!("a call to {callee}, which wasn't inlined");
            }
        }
    }
    let tree = structure(f)?;
    let mut g = Gen { m, f, info: &info, launch, out: String::new(), bases: HashMap::new(), atomic: BTreeSet::new(), private: BTreeMap::new(), depth: 1, temp: 0, needs: Default::default() };
    g.provenance()?;
    g.find_atomic_bases()?;
    // The body first: it finds the bases it uses and the helpers it needs.
    let mut body = String::new();
    std::mem::swap(&mut g.out, &mut body);
    g.node(&tree, &mut Vec::new())?;
    std::mem::swap(&mut g.out, &mut body);
    g.module(&body)
}

struct Gen<'a> {
    m: &'a Module,
    f: &'a Function,
    info: &'a KernelInfo,
    launch: &'a Launch,
    out: String,
    /// Each pointer value's base.
    bases: HashMap<Value, Base>,
    atomic: BTreeSet<Base>,
    /// Allocas: their size in words.
    private: BTreeMap<usize, u64>,
    depth: usize,
    temp: usize,
    /// Library functions the code calls (`LIBRARY`).
    needs: std::cell::RefCell<BTreeSet<&'static str>>,
}

// Values as map keys: by their identity.
impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        match self {
            Value::Local(n) => (0u8, *n).hash(h),
            Value::Arg(n) => (1u8, *n).hash(h),
            Value::Const(c) => (2u8, format!("{c:?}")).hash(h),
        }
    }
}
impl Eq for Value {}

impl Gen<'_> {
    fn line(&mut self, s: &str) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    // ---- Where pointers point ----

    fn provenance(&mut self) -> Res<()> {
        for (k, t) in self.f.params.iter().enumerate() {
            match t {
                Type::Ptr(1) | Type::Ptr(2) => {
                    self.bases.insert(Value::Arg(k), Base::Buffer(k));
                }
                Type::Ptr(3) => {
                    self.bases.insert(Value::Arg(k), Base::LocalArg(k));
                }
                Type::Ptr(0) => {
                    self.bases.insert(Value::Arg(k), Base::Block);
                }
                _ => {}
            }
        }
        // Until nothing changes: phis may use values defined later.
        loop {
            let before = self.bases.len();
            for b in &self.f.blocks {
                for i in &b.insts {
                    let Some(r) = i.result else { continue };
                    if !matches!(i.ty, Type::Ptr(_)) {
                        continue;
                    }
                    let base = match &i.op {
                        Op::Alloca(t, n, _) => {
                            let (s, _) = self.m.layout(t)?;
                            self.private.insert(r, (s * n).div_ceil(4).max(1));
                            Some(Base::Private(r))
                        }
                        Op::Gep(_, p, _) | Op::Cast(_, p) | Op::Freeze(p) => self.base_of(&p.v),
                        Op::Phi(inc) => self.common_base(inc.iter().map(|(o, _)| &o.v))?,
                        Op::Select(_, a, b) => self.common_base([&a.v, &b.v].into_iter())?,
                        _ => None,
                    };
                    if let Some(base) = base {
                        self.bases.insert(Value::Local(r), base);
                    }
                }
            }
            if self.bases.len() == before {
                return Ok(());
            }
        }
    }

    fn base_of(&self, v: &Value) -> Option<Base> {
        match v {
            Value::Const(Const::Global(n)) => self.m.globals.iter().position(|g| &g.name == n).map(Base::Global),
            Value::Const(Const::Gep(_, ops)) => ops.first().and_then(|o| self.base_of(&o.v)),
            Value::Const(Const::Cast(_, o)) => self.base_of(&o.v),
            v => self.bases.get(v).copied(),
        }
    }

    fn common_base<'v>(&self, vs: impl Iterator<Item = &'v Value>) -> Res<Option<Base>> {
        let mut base = None;
        for v in vs {
            if matches!(v, Value::Const(Const::Undef | Const::Null)) {
                continue;
            }
            match (base, self.base_of(v)) {
                (_, None) => {}
                (None, b) => base = b,
                (Some(a), Some(b)) if a != b => bail!("a pointer that may point into different buffers"),
                _ => {}
            }
        }
        Ok(base)
    }

    fn base(&self, p: &Operand) -> Res<Base> {
        self.base_of(&p.v).ok_or_else(|| format!("a pointer whose buffer isn't known ({:?})", p.v))
    }

    fn find_atomic_bases(&mut self) -> Res<()> {
        for b in &self.f.blocks {
            for i in &b.insts {
                match &i.op {
                    Op::AtomicRmw(_, p, _) | Op::CmpXchg(p, _, _) => {
                        let b = self.base(p)?;
                        self.atomic.insert(b);
                    }
                    Op::Store(v, p, _) => {
                        let b = self.base(p)?;
                        let (size, _) = self.m.layout(&v.ty)?;
                        if size % 4 != 0 && matches!(b, Base::Buffer(_) | Base::LocalArg(_) | Base::Global(_)) {
                            self.atomic.insert(b);
                        }
                    }
                    Op::Call(name, args) if atomic_builtin(name) => {
                        let b = self.base(&args[0])?;
                        self.atomic.insert(b);
                    }
                    Op::Call(name, args) if name.starts_with("llvm.memcpy") || name.starts_with("llvm.memset") || name.starts_with("llvm.memmove") => {
                        let b = self.base(&args[0])?;
                        if matches!(b, Base::Buffer(_) | Base::LocalArg(_) | Base::Global(_)) {
                            self.atomic.insert(b);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    // ---- The module around the body ----

    fn module(&self, body: &str) -> Res<String> {
        let mut o = String::new();
        let bases = self.used_bases();
        o.push_str("@group(0) @binding(0) var<storage, read> args: array<u32>;\n");
        for b in &bases {
            let atomic = self.atomic.contains(b);
            let word = if atomic { "atomic<u32>" } else { "u32" };
            match *b {
                Base::Block => {}
                Base::Buffer(k) => writeln!(o, "@group(0) @binding({}) var<storage, read_write> {}: array<{word}>;", k + 1, name(*b)).unwrap(),
                Base::LocalArg(k) => {
                    let bytes = self.launch.local_bytes.iter().find(|(a, _)| *a == k).map_or(4, |(_, n)| *n);
                    writeln!(o, "var<workgroup> {}: array<{word}, {}>;", name(*b), bytes.div_ceil(4).max(1)).unwrap();
                }
                Base::Private(r) => writeln!(o, "var<private> {}: array<u32, {}>;", name(*b), self.private[&r]).unwrap(),
                Base::Global(k) => {
                    let g = &self.m.globals[k];
                    let (size, _) = self.m.layout(&g.ty)?;
                    let words = size.div_ceil(4).max(1);
                    match g.space {
                        3 => writeln!(o, "var<workgroup> {}: array<{word}, {words}>;", name(*b)).unwrap(),
                        0 | 2 => {
                            let mut bytes = vec![0u8; (words * 4) as usize];
                            if let Some(init) = &g.init {
                                self.const_bytes(&g.ty, init, &mut bytes, 0)?;
                            }
                            let items: Vec<String> = bytes.chunks(4).map(|c| format!("{}u", u32::from_le_bytes(c.try_into().unwrap()))).collect();
                            writeln!(o, "var<private> {}: array<u32, {words}> = array<u32, {words}>({});", name(*b), items.join(", ")).unwrap();
                        }
                        s => bail!("a program-scope variable in address space {s}"),
                    }
                }
            }
        }
        for b in &bases {
            self.helpers(*b, &mut o);
        }
        for (n, code) in LIBRARY {
            if self.needs.borrow().contains(n) {
                o.push_str(code);
                o.push('\n');
            }
        }
        let [x, y, z] = self.launch.local;
        writeln!(o, "\n@compute @workgroup_size({x}, {y}, {z})").unwrap();
        o.push_str("fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {\n");
        writeln!(o, "    let grp = wid + vec3<u32>(args[{GROUP_OFFSET}], args[{}], args[{}]);", GROUP_OFFSET + 1, GROUP_OFFSET + 2).unwrap();
        writeln!(o, "    let gid = grp * vec3<u32>({x}u, {y}u, {z}u) + lid + vec3<u32>(args[{GLOBAL_OFFSET}], args[{}], args[{}]);", GLOBAL_OFFSET + 1, GLOBAL_OFFSET + 2)
            .unwrap();
        o.push_str("    var jump: u32 = 0u;\n");
        for (k, t) in self.f.params.iter().enumerate() {
            let a = &self.info.args[k];
            let v = match (&a.kind, t) {
                (ArgKind::Buffer { .. }, _) => format!("args[{}]", a.offset / 4),
                (ArgKind::Local, _) => "0u".into(),
                (_, Type::Ptr(0)) => format!("{}u", a.offset),
                (_, t) => self.load_expr(Base::Block, &format!("{}u", a.offset), t, 4)?,
            };
            writeln!(o, "    let a{k} = {v};").unwrap();
        }
        for (r, t) in self.f.values.iter().enumerate() {
            if let Some(ty) = self.wty_opt(t)? {
                writeln!(o, "    var v{r}: {ty};").unwrap();
            } else if let Type::Struct(fields, _) = t
                && fields.len() == 2
            {
                // cmpxchg's { value, success }.
                writeln!(o, "    var v{r}_0: {};\n    var v{r}_1: bool;", self.wty(&fields[0])?).unwrap();
            }
        }
        o.push_str(body);
        o.push_str("}\n");
        Ok(o)
    }

    fn used_bases(&self) -> BTreeSet<Base> {
        let mut s: BTreeSet<Base> = self.bases.values().copied().collect();
        s.insert(Base::Block);
        s.extend(self.atomic.iter().copied());
        s
    }

    /// Per-base helpers: a 32-bit load at any byte offset, and a store of
    /// 1, 2 or 4 bytes at any offset.
    fn helpers(&self, b: Base, o: &mut String) {
        let n = name(b);
        let atomic = self.atomic.contains(&b);
        let rd = |i: &str| if atomic { format!("atomicLoad(&{n}[{i}])") } else { format!("{n}[{i}]") };
        writeln!(
            o,
            "fn ld_{n}(p: u32) -> u32 {{\n    let i = p >> 2u;\n    let s = (p & 3u) * 8u;\n    if (s == 0u) {{ return {}; }}\n    return ({} >> s) | ({} << (32u - s));\n}}",
            rd("i"),
            rd("i"),
            rd("i + 1u")
        )
        .unwrap();
        if b == Base::Block {
            return;
        }
        let set_byte = if atomic {
            format!("atomicAnd(&{n}[i], ~(0xffu << s));\n        atomicOr(&{n}[i], ((v >> (8u * k)) & 0xffu) << s);")
        } else {
            format!("{n}[i] = ({n}[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);")
        };
        let whole = if atomic { format!("atomicStore(&{n}[p >> 2u], v)") } else { format!("{n}[p >> 2u] = v") };
        writeln!(
            o,
            "fn st_{n}(p: u32, v: u32, bytes: u32) {{\n    if ((p & 3u) == 0u && bytes == 4u) {{ {whole}; return; }}\n    for (var k = 0u; k < bytes; k++) {{\n        let q = p + k;\n        let i = q >> 2u;\n        let s = (q & 3u) * 8u;\n        {set_byte}\n    }}\n}}"
        )
        .unwrap();
    }

    fn const_bytes(&self, t: &Type, c: &Const, out: &mut [u8], at: usize) -> Res<()> {
        let t = self.m.resolve(t)?.clone();
        match (c, &t) {
            (Const::Zero | Const::Undef | Const::Null, _) => {}
            (Const::Int(n), Type::Int(_)) => {
                let (size, _) = self.m.layout(&t)?;
                out[at..at + size as usize].copy_from_slice(&n.to_le_bytes()[..size as usize]);
            }
            (Const::Float(x), Type::Float) => out[at..at + 4].copy_from_slice(&(*x as f32).to_le_bytes()),
            (Const::Bytes(b), _) => out[at..at + b.len()].copy_from_slice(b),
            (Const::Array(items), Type::Array(_, e)) | (Const::Vector(items), Type::Vector(_, e)) => {
                let (s, _) = self.m.layout(e)?;
                for (i, item) in items.iter().enumerate() {
                    self.const_bytes(e, item, out, at + i * s as usize)?;
                }
            }
            (Const::Struct(items), Type::Struct(fields, _)) => {
                for (i, (item, ft)) in items.iter().zip(fields).enumerate() {
                    let off = self.m.field_offset(&t, i)? as usize;
                    self.const_bytes(ft, item, out, at + off)?;
                }
            }
            (c, t) => bail!("an initializer {c:?} of {t:?}"),
        }
        Ok(())
    }

    // ---- Types and values ----

    fn wty(&self, t: &Type) -> Res<String> {
        self.wty_opt(t)?.ok_or_else(|| format!("values of type {t:?}"))
    }

    fn wty_opt(&self, t: &Type) -> Res<Option<String>> {
        Ok(Some(match self.m.resolve(t)? {
            Type::Void | Type::Struct(..) | Type::Array(..) | Type::Label | Type::Metadata => return Ok(None),
            Type::Int(1) => "bool".into(),
            Type::Int(8 | 16 | 32) | Type::Ptr(_) => "u32".into(),
            Type::Float => "f32".into(),
            Type::Vector(n @ 2..=4, e) => format!("vec{n}<{}>", self.wty(e)?),
            t => bail!("values of type {t:?}"),
        }))
    }

    /// A value as a WGSL expression.
    fn val(&self, o: &Operand) -> Res<String> {
        match &o.v {
            Value::Local(n) => Ok(format!("v{n}")),
            Value::Arg(n) => Ok(format!("a{n}")),
            Value::Const(c) => self.constant(&o.ty, c),
        }
    }

    fn constant(&self, t: &Type, c: &Const) -> Res<String> {
        let t = self.m.resolve(t)?;
        Ok(match (c, t) {
            (Const::Int(n), Type::Int(1)) => (if n & 1 != 0 { "true" } else { "false" }).into(),
            (Const::Int(n), Type::Int(b)) => format!("{}u", n & mask(*b) as u64),
            (Const::Float(x), Type::Float) => float(*x as f32),
            (Const::Undef | Const::Zero | Const::Null, Type::Ptr(_)) => "0u".into(),
            (Const::Undef | Const::Zero | Const::Null, t) => format!("{}()", self.wty(t)?),
            (Const::Vector(items), Type::Vector(_, e)) => {
                let items: Vec<String> = items.iter().map(|i| self.constant(e, i)).collect::<Res<_>>()?;
                format!("{}({})", self.wty(t)?, items.join(", "))
            }
            (Const::Global(_), Type::Ptr(_)) => "0u".into(),
            (Const::Gep(src, ops), Type::Ptr(_)) => {
                let base = self.val(&ops[0])?;
                let off = self.gep_offset(src, &ops[1..])?;
                format!("({base} + {off})")
            }
            (Const::Cast(_, o), _) => self.val(o)?,
            (c, t) => bail!("the constant {c:?} of {t:?}"),
        })
    }

    /// The byte offset a GEP's indices add.
    fn gep_offset(&self, src: &Type, idx: &[Operand]) -> Res<String> {
        let mut terms = Vec::new();
        let mut t = src.clone();
        for (k, i) in idx.iter().enumerate() {
            let size = if k == 0 {
                self.m.layout(&t)?.0
            } else {
                match self.m.resolve(&t)?.clone() {
                    Type::Struct(fields, _) => {
                        let Value::Const(Const::Int(f)) = i.v else { bail!("a struct index that isn't constant") };
                        terms.push(format!("{}u", self.m.field_offset(&t, f as usize)?));
                        t = fields[f as usize].clone();
                        continue;
                    }
                    Type::Array(_, e) | Type::Vector(_, e) => {
                        t = *e;
                        self.m.layout(&t)?.0
                    }
                    t => bail!("a GEP into {t:?}"),
                }
            };
            match &i.v {
                Value::Const(Const::Int(n)) => {
                    let n = sext(*n, bits(&i.ty));
                    terms.push(format!("{}u", (n.wrapping_mul(size as i64)) as u32));
                }
                _ => {
                    let x = self.int32(i)?;
                    terms.push(if size == 1 { x } else { format!("{x} * {size}u") });
                }
            }
        }
        Ok(if terms.is_empty() { "0u".into() } else { terms.join(" + ") })
    }

    /// An integer operand as a u32 sign-extended from its width (GEP
    /// indices are signed).
    fn int32(&self, o: &Operand) -> Res<String> {
        let b = bits(&o.ty);
        let v = self.val(o)?;
        Ok(match b {
            32 => v,
            64 => bail!("64-bit integers"),
            b => sext_expr(&v, b),
        })
    }

    // ---- Structured code ----

    fn node(&mut self, n: &Node, stack: &mut Vec<(bool, usize)>) -> Res<()> {
        match n {
            Node::Code(b) => {
                for i in &self.f.blocks[*b].insts {
                    self.inst(i)?;
                }
            }
            Node::Seq(items) => {
                for i in items {
                    self.node(i, stack)?;
                }
            }
            Node::Block(label, body) | Node::Loop(label, body) => {
                let is_loop = matches!(n, Node::Loop(..));
                self.line("loop {");
                self.depth += 1;
                stack.push((is_loop, *label));
                self.node(body, stack)?;
                stack.pop();
                if !is_loop {
                    self.line("break;");
                }
                self.depth -= 1;
                self.line("}");
                // Jumps from inside to constructs further out.
                let mut outer = BTreeSet::new();
                targets(body, &mut outer);
                outer.remove(label);
                if !outer.is_empty() {
                    let Some(&(parent_loop, parent)) = stack.last() else { bail!("a jump out of the function") };
                    self.line("if (jump != 0u) {");
                    self.depth += 1;
                    if outer.contains(&parent) {
                        self.line(&format!("if (jump == {}u) {{ jump = 0u; {} }}", parent + 1, if parent_loop { "continue;" } else { "break;" }));
                    }
                    if outer.iter().any(|&l| l != parent) {
                        self.line("break;");
                    }
                    self.depth -= 1;
                    self.line("}");
                }
            }
            Node::If(c, a, b) => {
                let c = self.val(c)?;
                self.line(&format!("if ({c}) {{"));
                self.depth += 1;
                self.node(a, stack)?;
                self.depth -= 1;
                self.line("} else {");
                self.depth += 1;
                self.node(b, stack)?;
                self.depth -= 1;
                self.line("}");
            }
            Node::Case(v, k, a, b) => {
                let x = self.val(v)?;
                let k = self.constant(&v.ty, &Const::Int(*k))?;
                self.line(&format!("if ({x} == {k}) {{"));
                self.depth += 1;
                self.node(a, stack)?;
                self.depth -= 1;
                self.line("} else {");
                self.depth += 1;
                self.node(b, stack)?;
                self.depth -= 1;
                self.line("}");
            }
            Node::Jump { from, to } => {
                self.phis(*from, *to)?;
                match stack.last() {
                    Some(&(true, l)) if l == *to => self.line("continue;"),
                    Some(&(false, l)) if l == *to => self.line("break;"),
                    _ => self.line(&format!("jump = {}u; break;", to + 1)),
                }
            }
            Node::Edge { from, to } => self.phis(*from, *to)?,
            Node::Return(_) | Node::Unreachable => self.line("return;"),
        }
        Ok(())
    }

    /// `to`'s phis take their values for the edge from `from`, all at once.
    fn phis(&mut self, from: usize, to: usize) -> Res<()> {
        let mut copies = Vec::new();
        for i in &self.f.blocks[to].insts {
            let Op::Phi(inc) = &i.op else { continue };
            let Some((v, _)) = inc.iter().find(|(_, b)| *b == from) else { bail!("a phi without a value for an edge") };
            if self.wty_opt(&i.ty)?.is_some() {
                copies.push((i.result.unwrap(), self.val(v)?));
            }
        }
        match copies.len() {
            0 => {}
            1 => self.line(&format!("v{} = {};", copies[0].0, copies[0].1)),
            _ => {
                self.line("{");
                for (k, (_, v)) in copies.iter().enumerate() {
                    self.line(&format!("    let t{k} = {v};"));
                }
                for (k, (r, _)) in copies.iter().enumerate() {
                    self.line(&format!("    v{r} = t{k};"));
                }
                self.line("}");
            }
        }
        Ok(())
    }

    // ---- Instructions ----

    fn set(&mut self, i: &Inst, e: String) {
        if let Some(r) = i.result {
            self.line(&format!("v{r} = {e};"));
        }
    }

    fn inst(&mut self, i: &Inst) -> Res<()> {
        match &i.op {
            Op::Phi(_) => {}
            Op::Bin(op, a, b) => {
                let e = self.binary(op, a, b)?;
                self.set(i, e);
            }
            Op::FNeg(a) => {
                let e = format!("-{}", self.val(a)?);
                self.set(i, e);
            }
            Op::ICmp(p, a, b) => {
                let e = self.icmp(p, a, b)?;
                self.set(i, e);
            }
            Op::FCmp(p, a, b) => {
                let e = self.fcmp(p, a, b)?;
                self.set(i, e);
            }
            Op::Select(c, a, b) => {
                let e = format!("select({}, {}, {})", self.val(b)?, self.val(a)?, self.val(c)?);
                self.set(i, e);
            }
            Op::Freeze(a) => {
                let e = self.val(a)?;
                self.set(i, e);
            }
            Op::Cast(op, a) => {
                let e = self.cast(op, a, &i.ty)?;
                self.set(i, e);
            }
            Op::Gep(src, base, idx) => {
                let e = format!("{} + {}", self.val(base)?, self.gep_offset(src, idx)?);
                self.set(i, e);
            }
            Op::Alloca(..) => self.set(i, "0u".into()),
            Op::Load(p, align) => {
                if matches!(i.ty, Type::Ptr(_)) {
                    bail!("a pointer loaded from memory");
                }
                let b = self.base(p)?;
                let e = self.load_expr(b, &self.val(p)?, &i.ty, *align)?;
                self.set(i, e);
            }
            Op::Store(v, p, align) => {
                if matches!(v.ty, Type::Ptr(_)) && self.base_of(&v.v).is_some() {
                    bail!("a pointer stored to memory");
                }
                let b = self.base(p)?;
                let (pv, vv) = (self.val(p)?, self.val(v)?);
                self.store(b, &pv, &v.ty, &vv, *align)?;
            }
            Op::ExtractElement(v, k) => {
                let e = format!("{}[{}]", self.val(v)?, self.index(k)?);
                self.set(i, e);
            }
            Op::InsertElement(v, e, k) => {
                let Some(r) = i.result else { return Ok(()) };
                let (v, e, k) = (self.val(v)?, self.val(e)?, self.index(k)?);
                self.line(&format!("v{r} = {v};"));
                self.line(&format!("v{r}[{k}] = {e};"));
            }
            Op::Shuffle(a, b, mask) => {
                let n = a.ty.lanes() as i64;
                let (av, bv) = (self.val(a)?, self.val(b)?);
                let zero = format!("{}()", self.wty(a.ty.scalar())?);
                let lanes: Vec<String> = mask
                    .iter()
                    .map(|&k| if k < 0 { zero.clone() } else if k < n { format!("{av}[{k}]") } else { format!("{bv}[{}]", k - n) })
                    .collect();
                let e = if lanes.len() == 1 { lanes[0].clone() } else { format!("{}({})", self.wty(&i.ty)?, lanes.join(", ")) };
                self.set(i, e);
            }
            Op::ExtractValue(v, idx) => {
                let (Value::Local(n), [k]) = (&v.v, idx.as_slice()) else { bail!("extractvalue of {:?}", v.v) };
                self.set(i, format!("v{n}_{k}"));
            }
            Op::InsertValue(..) => bail!("insertvalue"),
            Op::AtomicRmw(kind, p, v) => {
                let b = self.base(p)?;
                let pv = self.val(p)?;
                let vv = self.val(v)?;
                let e = self.atomic_rmw(b, kind, &pv, &vv, &v.ty)?;
                self.set(i, e);
            }
            Op::CmpXchg(p, cmp, new) => {
                let b = self.base(p)?;
                let (pv, c, nv) = (self.val(p)?, self.val(cmp)?, self.val(new)?);
                let r = i.result.ok_or("a cmpxchg without a result")?;
                let old = self.cas(b, &pv, &self.to_word(&c, &cmp.ty)?, &self.to_word(&nv, &new.ty)?);
                self.line(&format!("v{r}_0 = {};", self.from_word(&old, &cmp.ty)?));
                self.line(&format!("v{r}_1 = v{r}_0 == {c};"));
            }
            Op::Fence => {}
            Op::Call(name, args) => self.call(i, name, args)?,
        }
        Ok(())
    }

    fn index(&self, k: &Operand) -> Res<String> {
        Ok(match &k.v {
            Value::Const(Const::Int(n)) => format!("{n}"),
            _ => self.val(k)?,
        })
    }

    fn binary(&self, op: &str, a: &Operand, b: &Operand) -> Res<String> {
        let (x, y) = (self.val(a)?, self.val(b)?);
        let t = &a.ty;
        if t.scalar().is_float() {
            let o = match op {
                "fadd" => "+",
                "fsub" => "-",
                "fmul" => "*",
                "fdiv" => "/",
                "frem" => "%",
                o => bail!("{o} on floats"),
            };
            return Ok(format!("{x} {o} {y}"));
        }
        let w = bits(t);
        if w == 1 {
            // i1 as bool.
            return Ok(match op {
                "and" => format!("{x} && {y}"),
                "or" => format!("{x} || {y}"),
                "xor" | "add" | "sub" => format!("{x} != {y}"),
                "mul" => format!("{x} && {y}"),
                o => bail!("{o} on i1"),
            });
        }
        if w > 32 {
            bail!("64-bit integers");
        }
        let wrap = |e: String| if w < 32 { format!("(({e}) & {})", self.splat(t, &format!("{}u", mask(w)))) } else { e };
        let signed = |v: &str| sext_signed(v, w, t.lanes());
        let unsigned_of = |e: String| if t.lanes() > 1 { format!("vec{}<u32>({e})", t.lanes()) } else { format!("u32({e})") };
        Ok(match op {
            "add" => wrap(format!("{x} + {y}")),
            "sub" => wrap(format!("{x} - {y}")),
            "mul" => wrap(format!("{x} * {y}")),
            "shl" => wrap(format!("{x} << {y}")),
            "and" => format!("{x} & {y}"),
            "or" => format!("{x} | {y}"),
            "xor" => format!("{x} ^ {y}"),
            "lshr" => format!("{x} >> {y}"),
            "udiv" => format!("{x} / {y}"),
            "urem" => format!("{x} % {y}"),
            "ashr" => wrap(unsigned_of(format!("{} >> {y}", signed(&x)))),
            "sdiv" => wrap(unsigned_of(format!("{} / {}", signed(&x), signed(&y)))),
            "srem" => wrap(unsigned_of(format!("{} % {}", signed(&x), signed(&y)))),
            o => bail!("the operation {o}"),
        })
    }

    /// A scalar expression splatted to `t`'s lanes, if it's a vector.
    fn splat(&self, t: &Type, s: &str) -> String {
        if t.lanes() > 1 { format!("vec{}<u32>({s})", t.lanes()) } else { s.to_string() }
    }

    fn icmp(&self, p: &str, a: &Operand, b: &Operand) -> Res<String> {
        let (x, y) = (self.val(a)?, self.val(b)?);
        let w = bits(&a.ty);
        if w > 32 && !matches!(a.ty.scalar(), Type::Ptr(_)) {
            bail!("64-bit integers");
        }
        let (x, y) = if p.starts_with('s') && w < 32 && w > 1 {
            (sext_signed(&x, w, a.ty.lanes()), sext_signed(&y, w, a.ty.lanes()))
        } else if p.starts_with('s') && w == 32 {
            let c = if a.ty.lanes() > 1 { format!("vec{}<i32>", a.ty.lanes()) } else { "i32".into() };
            (format!("bitcast<{c}>({x})"), format!("bitcast<{c}>({y})"))
        } else {
            (x, y)
        };
        let o = match p {
            "eq" => "==",
            "ne" => "!=",
            "ult" | "slt" => "<",
            "ule" | "sle" => "<=",
            "ugt" | "sgt" => ">",
            "uge" | "sge" => ">=",
            p => bail!("icmp {p}"),
        };
        if w == 1 && !matches!(p, "eq" | "ne") {
            // Booleans ordered: false < true (unsigned) or true < false (signed).
            let (x, y) = if p.starts_with('s') { (y, x) } else { (x, y) };
            return Ok(format!("select(0u, 1u, {x}) {o} select(0u, 1u, {y})"));
        }
        Ok(format!("{x} {o} {y}"))
    }

    fn fcmp(&self, p: &str, a: &Operand, b: &Operand) -> Res<String> {
        let (x, y) = (self.val(a)?, self.val(b)?);
        let ord = format!("(({x} == {x}) && ({y} == {y}))");
        let c = |o: &str| format!("({x} {o} {y})");
        Ok(match p {
            "oeq" => c("=="),
            "one" => format!("({} && {ord})", c("!=")),
            "olt" => c("<"),
            "ole" => c("<="),
            "ogt" => c(">"),
            "oge" => c(">="),
            "ord" => ord,
            "uno" => format!("!{ord}"),
            "ueq" => format!("!({} || {} )", c("<"), c(">")),
            "une" => c("!="),
            "ult" => format!("!{}", c(">=")),
            "ule" => format!("!{}", c(">")),
            "ugt" => format!("!{}", c("<=")),
            "uge" => format!("!{}", c("<")),
            "true" => "true".into(),
            "false" => "false".into(),
            p => bail!("fcmp {p}"),
        })
    }

    fn cast(&self, op: &str, a: &Operand, to: &Type) -> Res<String> {
        let x = self.val(a)?;
        let (from_w, to_w) = (bits(&a.ty), bits(to));
        let lanes = to.lanes();
        let vt = |s: &str| if lanes > 1 { format!("vec{lanes}<{s}>") } else { s.to_string() };
        if (from_w > 32 && !a.ty.scalar().is_float() && !matches!(a.ty.scalar(), Type::Ptr(_))) || (to_w > 32 && !to.scalar().is_float() && !matches!(to.scalar(), Type::Ptr(_))) {
            bail!("64-bit integers");
        }
        if matches!(a.ty.scalar(), Type::Double | Type::Half) || matches!(to.scalar(), Type::Double | Type::Half) {
            bail!("{op} with {:?} or {to:?}", a.ty);
        }
        Ok(match op {
            "zext" if from_w == 1 => format!("select({}, {}, {x})", self.splat(to, "0u"), self.splat(to, "1u")),
            "zext" => x,
            "sext" if from_w == 1 => format!("select({}, {}, {x})", self.splat(to, "0u"), self.splat(to, &format!("{}u", mask(to_w)))),
            "sext" => {
                let e = format!("{}({})", vt("u32"), sext_signed(&x, from_w, lanes));
                if to_w < 32 { format!("({e} & {})", self.splat(to, &format!("{}u", mask(to_w)))) } else { e }
            }
            "trunc" if to_w == 1 => format!("({x} & {}) != {}", self.splat(to, "1u"), self.splat(to, "0u")),
            "trunc" => format!("({x} & {})", self.splat(to, &format!("{}u", mask(to_w)))),
            "fptoui" => {
                let e = format!("{}({x})", vt("u32"));
                if to_w < 32 { format!("({e} & {})", self.splat(to, &format!("{}u", mask(to_w)))) } else { e }
            }
            "fptosi" => {
                let e = format!("{}({}({x}))", vt("u32"), vt("i32"));
                if to_w < 32 { format!("({e} & {})", self.splat(to, &format!("{}u", mask(to_w)))) } else { e }
            }
            "uitofp" if from_w == 1 => format!("select({}(0.0), {}(1.0), {x})", vt("f32"), vt("f32")),
            "uitofp" => format!("{}({x})", vt("f32")),
            "sitofp" if from_w == 1 => format!("select({}(0.0), {}(-1.0), {x})", vt("f32"), vt("f32")),
            "sitofp" => format!("{}({})", vt("f32"), sext_signed(&x, from_w, lanes)),
            "fpext" | "fptrunc" => x,
            "bitcast" => {
                if a.ty.lanes() != to.lanes() {
                    bail!("a bitcast from {:?} to {to:?}", a.ty);
                }
                if a.ty.scalar().is_float() != to.scalar().is_float() { format!("bitcast<{}>({x})", self.wty(to)?) } else { x }
            }
            "ptrtoint" | "inttoptr" | "addrspacecast" => x,
            op => bail!("the cast {op}"),
        })
    }

    // ---- Memory ----

    /// A value's bits as a u32 word (floats bitcast, bools 0 or 1).
    fn to_word(&self, v: &str, t: &Type) -> Res<String> {
        Ok(match t {
            Type::Float => format!("bitcast<u32>({v})"),
            Type::Int(1) => format!("select(0u, 1u, {v})"),
            Type::Int(_) | Type::Ptr(_) => v.to_string(),
            t => bail!("a {t:?} as a word"),
        })
    }

    fn from_word(&self, w: &str, t: &Type) -> Res<String> {
        Ok(match t {
            Type::Float => format!("bitcast<f32>({w})"),
            Type::Int(1) => format!("(({w} & 1u) != 0u)"),
            Type::Int(b) if *b < 32 => format!("({w} & {}u)", mask(*b)),
            Type::Int(_) | Type::Ptr(_) => w.to_string(),
            t => bail!("a {t:?} from a word"),
        })
    }

    fn word_at(&self, b: Base, p: &str, align: u32) -> String {
        let n = name(b);
        if align.is_multiple_of(4) {
            if self.atomic.contains(&b) { format!("atomicLoad(&{n}[({p}) >> 2u])") } else { format!("{n}[({p}) >> 2u]") }
        } else {
            format!("ld_{n}({p})")
        }
    }

    fn load_expr(&self, b: Base, p: &str, t: &Type, align: u32) -> Res<String> {
        let t = self.m.resolve(t)?;
        match t {
            Type::Vector(n, e) => {
                let (s, _) = self.m.layout(e)?;
                let lanes: Vec<String> =
                    (0..*n).map(|k| self.load_expr(b, &format!("({p} + {}u)", k as u64 * s), e, if align as u64 >= s { s as u32 } else { align })).collect::<Res<_>>()?;
                Ok(format!("{}({})", self.wty(t)?, lanes.join(", ")))
            }
            _ => {
                let (s, _) = self.m.layout(t)?;
                let w = if s >= 4 { self.word_at(b, p, align) } else { format!("ld_{}({p})", name(b)) };
                self.from_word(&w, t)
            }
        }
    }

    fn store(&mut self, b: Base, p: &str, t: &Type, v: &str, align: u32) -> Res<()> {
        let t = self.m.resolve(t)?.clone();
        match &t {
            Type::Vector(n, e) => {
                let (s, _) = self.m.layout(e)?;
                for k in 0..*n {
                    self.store(b, &format!("({p} + {}u)", k as u64 * s), e, &format!("{v}[{k}]"), if align as u64 >= s { s as u32 } else { align })?;
                }
            }
            _ => {
                let (s, _) = self.m.layout(&t)?;
                let w = self.to_word(v, &t)?;
                let n = name(b);
                if s == 4 && align.is_multiple_of(4) {
                    if self.atomic.contains(&b) {
                        self.line(&format!("atomicStore(&{n}[({p}) >> 2u], {w});"));
                    } else {
                        self.line(&format!("{n}[({p}) >> 2u] = {w};"));
                    }
                } else {
                    self.line(&format!("st_{n}({p}, {w}, {s}u);"));
                }
            }
        }
        Ok(())
    }

    /// A strong compare-and-swap of a word: its old value.
    fn cas(&mut self, b: Base, p: &str, cmp: &str, new: &str) -> String {
        let n = name(b);
        let t = self.temp;
        self.temp += 1;
        self.line(&format!("var old{t}: u32;"));
        self.line("loop {");
        self.line(&format!("    let r = atomicCompareExchangeWeak(&{n}[({p}) >> 2u], {cmp}, {new});"));
        self.line(&format!("    if (r.exchanged || r.old_value != {cmp}) {{ old{t} = r.old_value; break; }}"));
        self.line("}");
        format!("old{t}")
    }

    fn atomic_rmw(&mut self, b: Base, kind: &str, p: &str, v: &str, t: &Type) -> Res<String> {
        let n = name(b);
        let at = format!("&{n}[({p}) >> 2u]");
        let w = self.to_word(v, t)?;
        let simple = match kind {
            "add" => Some("atomicAdd"),
            "sub" => Some("atomicSub"),
            "and" => Some("atomicAnd"),
            "or" => Some("atomicOr"),
            "xor" => Some("atomicXor"),
            "umin" => Some("atomicMin"),
            "umax" => Some("atomicMax"),
            "xchg" => Some("atomicExchange"),
            _ => None,
        };
        if let Some(f) = simple {
            return self.from_word(&format!("{f}({at}, {w})"), t);
        }
        // The rest by compare-and-swap.
        let tmp = self.temp;
        self.temp += 1;
        self.line(&format!("var old{tmp}: u32 = atomicLoad({at});"));
        self.line("loop {");
        let cur = format!("old{tmp}");
        let new = match kind {
            "min" => format!("bitcast<u32>(min(bitcast<i32>({cur}), bitcast<i32>({w})))"),
            "max" => format!("bitcast<u32>(max(bitcast<i32>({cur}), bitcast<i32>({w})))"),
            "fadd" => format!("bitcast<u32>(bitcast<f32>({cur}) + {v})"),
            "fsub" => format!("bitcast<u32>(bitcast<f32>({cur}) - {v})"),
            "fmin" => format!("bitcast<u32>(min(bitcast<f32>({cur}), {v}))"),
            "fmax" => format!("bitcast<u32>(max(bitcast<f32>({cur}), {v}))"),
            "uinc_wrap" => format!("select({cur} + 1u, 0u, {cur} >= {w})"),
            "udec_wrap" => format!("select({cur} - 1u, {w}, {cur} == 0u || {cur} > {w})"),
            k => bail!("atomicrmw {k}"),
        };
        self.line(&format!("    let r = atomicCompareExchangeWeak({at}, {cur}, {new});"));
        self.line(&format!("    if (r.exchanged) {{ break; }}"));
        self.line(&format!("    {cur} = r.old_value;"));
        self.line("}");
        self.from_word(&cur, t)
    }

    // ---- Calls: OpenCL's builtins and LLVM's intrinsics ----

    fn call(&mut self, i: &Inst, name: &str, args: &[Operand]) -> Res<()> {
        // (Metadata arguments, to intrinsics that don't matter here, have no value.)
        let vals: Vec<String> = args.iter().map(|a| if a.ty == Type::Metadata { Ok(String::new()) } else { self.val(a) }).collect::<Res<_>>()?;
        if let Some(intr) = name.strip_prefix("llvm.") {
            return self.intrinsic(i, intr, args, &vals);
        }
        let (base, params) = demangle(name).ok_or_else(|| format!("a call to {name}"))?;
        let unsigned = params.first().is_some_and(|p| matches!(p, 'h' | 't' | 'j' | 'm' | 'b'));
        let dim = |g: &Self, what: &str, v: &str| -> String {
            let _ = g;
            format!("select({what}, {v}, {} < 3u)", vals[0])
        };
        let e = match base.as_str() {
            "get_global_id" => dim(self, "0u", &format!("gid[min({}, 2u)]", vals[0])),
            "get_local_id" => dim(self, "0u", &format!("lid[min({}, 2u)]", vals[0])),
            "get_group_id" => dim(self, "0u", &format!("grp[min({}, 2u)]", vals[0])),
            "get_local_size" | "get_enqueued_local_size" => {
                let [x, y, z] = self.launch.local;
                dim(self, "1u", &format!("vec3<u32>({x}u, {y}u, {z}u)[min({}, 2u)]", vals[0]))
            }
            "get_global_size" => dim(self, "1u", &format!("args[{GLOBAL_SIZE}u + min({}, 2u)]", vals[0])),
            "get_global_offset" => dim(self, "0u", &format!("args[{GLOBAL_OFFSET}u + min({}, 2u)]", vals[0])),
            "get_num_groups" => {
                let [x, y, z] = self.launch.local;
                dim(self, "1u", &format!("(args[{GLOBAL_SIZE}u + min({v}, 2u)] / vec3<u32>({x}u, {y}u, {z}u)[min({v}, 2u)])", v = vals[0]))
            }
            "get_work_dim" => format!("args[{WORK_DIM}]"),
            "barrier" | "work_group_barrier" => {
                self.line("workgroupBarrier();");
                if !matches!(args.first().map(|a| &a.v), Some(Value::Const(Const::Int(1)))) {
                    self.line("storageBarrier();");
                }
                return Ok(());
            }
            "mem_fence" | "read_mem_fence" | "write_mem_fence" => return Ok(()),
            b if atomic_builtin(name) => {
                let op = b.trim_start_matches("atomic_").trim_start_matches("atom_");
                let bse = self.base(&args[0])?;
                let t = &i.ty;
                let e = match op {
                    "inc" => self.atomic_rmw(bse, "add", &vals[0], "1u", t)?,
                    "dec" => self.atomic_rmw(bse, "sub", &vals[0], "1u", t)?,
                    "cmpxchg" => {
                        let (c, n) = (self.to_word(&vals[1], &args[1].ty)?, self.to_word(&vals[2], &args[2].ty)?);
                        let old = self.cas(bse, &vals[0], &c, &n);
                        self.from_word(&old, t)?
                    }
                    "min" | "max" if unsigned => self.atomic_rmw(bse, &format!("u{op}"), &vals[0], &vals[1], &args[1].ty)?,
                    "xchg" if args[1].ty == Type::Float => self.atomic_rmw(bse, "xchg", &vals[0], &vals[1], &args[1].ty)?,
                    op => self.atomic_rmw(bse, op, &vals[0], &vals[1], &args[1].ty)?,
                };
                e
            }
            b if b.starts_with("vstore") => {
                // vstoreN(v, offset, p): N elements at p + offset * N.
                let n = args[0].ty.lanes();
                let bse = self.base(&args[2])?;
                let (s, _) = self.m.layout(args[0].ty.scalar())?;
                let p = format!("({} + {} * {}u)", vals[2], vals[1], n as u64 * s);
                return self.store(bse, &p, &args[0].ty, &vals[0], s as u32);
            }
            b => self.math(b, i, args, &vals, unsigned)?,
        };
        self.set(i, e);
        Ok(())
    }

    fn intrinsic(&mut self, i: &Inst, name: &str, args: &[Operand], v: &[String]) -> Res<()> {
        let base = name.split('.').next().unwrap_or(name);
        let t = &i.ty;
        let e = match base {
            "lifetime" | "assume" | "experimental" | "dbg" | "invariant" => return Ok(()),
            "fmuladd" | "fma" => format!("fma({}, {}, {})", v[0], v[1], v[2]),
            "fabs" => format!("abs({})", v[0]),
            "sqrt" | "floor" | "ceil" | "trunc" | "exp2" | "log2" | "exp" | "log" | "sin" | "cos" | "pow" => format!("{base}({})", v.join(", ")),
            "rint" | "nearbyint" | "roundeven" => format!("round({})", v[0]),
            "round" => format!("trunc({x} + sign({x}) * 0.5)", x = v[0]),
            "minnum" | "minimum" => format!("min({}, {})", v[0], v[1]),
            "maxnum" | "maximum" => format!("max({}, {})", v[0], v[1]),
            "copysign" => {
                let lanes = t.lanes();
                let u = if lanes > 1 { format!("vec{lanes}<u32>") } else { "u32".into() };
                let tw = self.wty(t)?;
                format!(
                    "bitcast<{tw}>((bitcast<{u}>({}) & {}) | (bitcast<{u}>({}) & {}))",
                    v[0],
                    self.splat(t, "0x7fffffffu"),
                    v[1],
                    self.splat(t, "0x80000000u")
                )
            }
            "umin" => format!("min({}, {})", v[0], v[1]),
            "umax" => format!("max({}, {})", v[0], v[1]),
            "smin" | "smax" => {
                let w = bits(t);
                let f = &base[1..];
                let e = format!("{f}({}, {})", sext_signed(&v[0], w, t.lanes()), sext_signed(&v[1], w, t.lanes()));
                self.wrap_signed(t, e)
            }
            "abs" => {
                let w = bits(t);
                self.wrap_signed(t, format!("abs({})", sext_signed(&v[0], w, t.lanes())))
            }
            "ctpop" => format!("countOneBits({})", v[0]),
            "ctlz" => {
                let w = bits(t);
                format!("(countLeadingZeros({}) - {})", v[0], self.splat(t, &format!("{}u", 32 - w)))
            }
            "cttz" => {
                let w = bits(t);
                format!("min(countTrailingZeros({}), {})", v[0], self.splat(t, &format!("{w}u")))
            }
            "bitreverse" => {
                let w = bits(t);
                format!("(reverseBits({}) >> {})", v[0], self.splat(t, &format!("{}u", 32 - w)))
            }
            "bswap" => {
                let w = bits(t);
                let x = &v[0];
                let s = |n: u32| self.splat(t, &format!("{n}u"));
                match w {
                    16 => format!("((({x} & {}) << {}) | ({x} >> {}))", s(0xff), s(8), s(8)),
                    32 => format!(
                        "(({x} << {}) | (({x} & {}) << {}) | (({x} >> {}) & {}) | ({x} >> {}))",
                        s(24),
                        s(0xff00),
                        s(8),
                        s(8),
                        s(0xff00),
                        s(24)
                    ),
                    w => bail!("bswap of i{w}"),
                }
            }
            "fshl" | "fshr" => {
                let w = bits(t);
                let s = |n: u32| self.splat(t, &format!("{n}u"));
                let sh = format!("({} % {})", v[2], s(w));
                let m = s(mask(w));
                if base == "fshl" {
                    format!("select((({a} << {sh}) | ({b} >> ({w_} - {sh}))) & {m}, {a}, {sh} == {z})", a = v[0], b = v[1], w_ = s(w), z = s(0))
                } else {
                    format!("select((({b} >> {sh}) | ({a} << ({w_} - {sh}))) & {m}, {b}, {sh} == {z})", a = v[0], b = v[1], w_ = s(w), z = s(0))
                }
            }
            "memcpy" | "memmove" | "memset" => {
                let (db, sb) = (self.base(&args[0])?, if base == "memset" { None } else { Some(self.base(&args[1])?) });
                let len = self.val(&args[2])?;
                let (dn, d) = (name_of(db), &v[0]);
                self.line(&format!("for (var k = 0u; k < {len}; k++) {{"));
                match sb {
                    Some(sb) => {
                        let (sn, s) = (name_of(sb), &v[1]);
                        self.line(&format!("    st_{dn}({d} + k, ld_{sn}({s} + k) & 0xffu, 1u);"));
                    }
                    None => self.line(&format!("    st_{dn}({d} + k, {} & 0xffu, 1u);", v[1])),
                }
                self.line("}");
                return Ok(());
            }
            n => bail!("the intrinsic llvm.{n}"),
        };
        self.set(i, e);
        Ok(())
    }

    /// A signed result (i32 lanes) back to the zero-extended representation.
    fn wrap_signed(&self, t: &Type, e: String) -> String {
        let w = bits(t);
        let u = if t.lanes() > 1 { format!("vec{}<u32>", t.lanes()) } else { "u32".into() };
        if w < 32 { format!("({u}({e}) & {})", self.splat(t, &format!("{}u", mask(w)))) } else { format!("{u}({e})") }
    }

    fn math(&mut self, b: &str, i: &Inst, args: &[Operand], v: &[String], unsigned: bool) -> Res<String> {
        let t = &i.ty;
        let float = t.scalar().is_float() || args.first().is_some_and(|a| a.ty.scalar().is_float());
        let b = b.trim_start_matches("native_").trim_start_matches("half_");
        let f1 = |f: &str| format!("{f}({})", v[0]);
        Ok(match b {
            "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sinh" | "cosh" | "tanh" | "asinh" | "acosh" | "atanh" | "exp" | "exp2" | "log" | "log2"
            | "floor" | "ceil" | "trunc" | "sign" | "degrees" | "radians" | "length" | "normalize" | "fract"
                if float =>
            {
                f1(b)
            }
            "rsqrt" => f1("inverseSqrt"),
            "recip" => format!("(1.0 / {})", v[0]),
            "divide" => format!("({} / {})", v[0], v[1]),
            "fabs" => f1("abs"),
            "rint" => f1("round"),
            "round" => format!("trunc({x} + sign({x}) * 0.5)", x = v[0]),
            "exp10" => format!("exp2({} * 3.321928094887362)", v[0]),
            "log10" => format!("(log2({}) * 0.30102999566398120)", v[0]),
            "expm1" => format!("(exp({}) - 1.0)", v[0]),
            "log1p" => format!("log(1.0 + {})", v[0]),
            "cbrt" => format!("(sign({x}) * pow(abs({x}), {}))", self.fsplat(t, "0.3333333333333333"), x = v[0]),
            "pow" | "powr" => format!("pow({}, {})", v[0], v[1]),
            "pown" => format!("pow({}, {}(bitcast<i32>({})))", v[0], self.wty(t)?, v[1]),
            "rootn" => format!("pow({}, 1.0 / {}(bitcast<i32>({})))", v[0], self.wty(t)?, v[1]),
            "atan2" => format!("atan2({}, {})", v[0], v[1]),
            "hypot" => format!("sqrt({a} * {a} + {b} * {b})", a = v[0], b = v[1]),
            "fmin" | "min" if float => format!("min({}, {})", v[0], v[1]),
            "fmax" | "max" if float => format!("max({}, {})", v[0], v[1]),
            "fmod" => format!("({} % {})", v[0], v[1]),
            "fdim" => format!("max({} - {}, {}(0.0))", v[0], v[1], self.wty(t)?),
            "fma" | "mad" => format!("fma({}, {}, {})", v[0], v[1], v[2]),
            "mix" => format!("mix({}, {}, {})", v[0], v[1], v[2]),
            "clamp" if float => format!("clamp({}, {}, {})", v[0], v[1], v[2]),
            "step" => format!("step({}, {})", v[0], v[1]),
            "smoothstep" => format!("smoothstep({}, {}, {})", v[0], v[1], v[2]),
            "dot" => format!("dot({}, {})", v[0], v[1]),
            "cross" => format!("cross({}, {})", v[0], v[1]),
            "distance" => format!("distance({}, {})", v[0], v[1]),
            "copysign" => {
                let lanes = t.lanes();
                let u = if lanes > 1 { format!("vec{lanes}<u32>") } else { "u32".into() };
                format!(
                    "bitcast<{}>((bitcast<{u}>({}) & {}) | (bitcast<{u}>({}) & {}))",
                    self.wty(t)?,
                    v[0],
                    self.splat(t, "0x7fffffffu"),
                    v[1],
                    self.splat(t, "0x80000000u")
                )
            }
            "isnan" | "isinf" | "isfinite" | "signbit" | "isnormal" => {
                let lanes = args[0].ty.lanes();
                let u = if lanes > 1 { format!("vec{lanes}<u32>") } else { "u32".into() };
                let bitsx = format!("bitcast<{u}>({})", v[0]);
                let s = |n: u32| if lanes > 1 { format!("vec{lanes}<u32>({n}u)") } else { format!("{n}u") };
                let cond = match b {
                    "isnan" => format!("({bitsx} & {}) > {}", s(0x7fffffff), s(0x7f800000)),
                    "isinf" => format!("({bitsx} & {}) == {}", s(0x7fffffff), s(0x7f800000)),
                    "isfinite" => format!("({bitsx} & {}) < {}", s(0x7fffffff), s(0x7f800000)),
                    "signbit" => format!("({bitsx} >> {}) != {}", s(31), s(0)),
                    _ => format!("(({bitsx} & {}) < {}) && (({bitsx} & {}) >= {})", s(0x7fffffff), s(0x7f800000), s(0x7fffffff), s(0x00800000)),
                };
                // Scalars: 1 or 0; vectors: -1 (all bits) or 0.
                let yes = if lanes > 1 { self.splat(t, &format!("{}u", mask(bits(t)))) } else { "1u".into() };
                format!("select({}, {yes}, {cond})", self.splat(t, "0u"))
            }
            "isequal" | "isnotequal" | "isgreater" | "isgreaterequal" | "isless" | "islessequal" => {
                let o = match b {
                    "isequal" => "==",
                    "isnotequal" => "!=",
                    "isgreater" => ">",
                    "isgreaterequal" => ">=",
                    "isless" => "<",
                    _ => "<=",
                };
                let yes = if t.lanes() > 1 { self.splat(t, &format!("{}u", mask(bits(t)))) } else { "1u".into() };
                format!("select({}, {yes}, {} {o} {})", self.splat(t, "0u"), v[0], v[1])
            }
            "select" => {
                // OpenCL's select(a, b, c): b where c (its top bit, for vectors).
                let c = &args[2];
                let cond = if c.ty.lanes() > 1 {
                    let w = bits(&c.ty);
                    format!("(({} >> {}) & {}) != {}", v[2], self.splat(&c.ty, &format!("{}u", w - 1)), self.splat(&c.ty, "1u"), self.splat(&c.ty, "0u"))
                } else {
                    format!("{} != 0u", v[2])
                };
                format!("select({}, {}, {cond})", v[0], v[1])
            }
            // Integers.
            "min" | "max" | "clamp" | "abs" | "abs_diff" | "add_sat" | "sub_sat" | "hadd" | "rhadd" | "mad24" | "mul24" | "mad_hi" | "mul_hi" | "rotate" | "clz" | "ctz"
            | "popcount" | "upsample" | "mad_sat" => self.integer(b, t, args, v, unsigned)?,
            b if b.starts_with("convert_") => self.convert(b, t, &args[0], &v[0])?,
            b if b.starts_with("vload") => {
                // vloadN(offset, p): N elements at p + offset * N.
                let n: u32 = b[5..].trim_start_matches("_half").parse().map_err(|_| format!("{b}"))?;
                let bse = self.base(&args[1])?;
                let e = t.scalar();
                let (s, _) = self.m.layout(e)?;
                let p = format!("({} + {} * {}u)", v[1], v[0], n as u64 * s);
                self.load_expr(bse, &p, t, s as u32)?
            }
            b => bail!("the builtin {b}"),
        })
    }

    fn fsplat(&self, t: &Type, x: &str) -> String {
        if t.lanes() > 1 { format!("vec{}<f32>({x})", t.lanes()) } else { x.to_string() }
    }

    fn integer(&self, b: &str, t: &Type, args: &[Operand], v: &[String], unsigned: bool) -> Res<String> {
        let w = bits(&args[0].ty);
        if w > 32 {
            bail!("64-bit integers");
        }
        let lanes = t.lanes();
        let s = |x: &str| sext_signed(x, w, lanes);
        let back = |e: String| self.wrap_signed(t, e);
        let m = |n: u64| self.splat(t, &format!("{n}u"));
        Ok(match (b, unsigned) {
            ("min", true) => format!("min({}, {})", v[0], v[1]),
            ("max", true) => format!("max({}, {})", v[0], v[1]),
            ("clamp", true) => format!("clamp({}, {}, {})", v[0], v[1], v[2]),
            ("min", false) => back(format!("min({}, {})", s(&v[0]), s(&v[1]))),
            ("max", false) => back(format!("max({}, {})", s(&v[0]), s(&v[1]))),
            ("clamp", false) => back(format!("clamp({}, {}, {})", s(&v[0]), s(&v[1]), s(&v[2]))),
            ("abs", true) => v[0].clone(),
            ("abs", false) => back(format!("abs({})", s(&v[0]))),
            ("abs_diff", true) => format!("(max({a}, {b}) - min({a}, {b}))", a = v[0], b = v[1]),
            ("abs_diff", false) => back(format!("abs({} - {})", s(&v[0]), s(&v[1]))),
            ("add_sat", true) if w < 32 => format!("min({} + {}, {})", v[0], v[1], m(mask(w) as u64)),
            ("add_sat", true) => format!("select({a} + {b}, {}, {a} + {b} < {a})", m(u32::MAX as u64), a = v[0], b = v[1]),
            ("sub_sat", true) => format!("select({a} - {b}, {}, {b} > {a})", m(0), a = v[0], b = v[1]),
            ("hadd", true) => format!("(({a} >> {one}) + ({b} >> {one}) + ({a} & {b} & {one}))", a = v[0], b = v[1], one = m(1)),
            ("rhadd", true) => format!("(({a} >> {one}) + ({b} >> {one}) + (({a} | {b}) & {one}))", a = v[0], b = v[1], one = m(1)),
            ("mul24", _) => format!("({} * {})", v[0], v[1]),
            ("mad24", _) => format!("({} * {} + {})", v[0], v[1], v[2]),
            ("clz", _) => format!("(countLeadingZeros({}) - {})", v[0], m(32 - w as u64)),
            ("ctz", _) => format!("min(countTrailingZeros({}), {})", v[0], m(w as u64)),
            ("popcount", _) => format!("countOneBits({})", v[0]),
            ("rotate", _) => {
                let sh = format!("({} % {})", v[1], m(w as u64));
                format!("((({a} << {sh}) | ({a} >> (({} - {sh}) % {}))) & {})", m(w as u64), m(w as u64), m(mask(w) as u64), a = v[0])
            }
            ("mul_hi", true) if w == 32 => {
                // The high word of a 32x32 product, from 16-bit halves.
                self.needs.borrow_mut().insert("mul_hi_u32");
                format!("mul_hi_u32({}, {})", v[0], v[1])
            }
            ("mul_hi", true) => format!("(({} * {}) >> {})", v[0], v[1], m(w as u64)),
            (b, u) => bail!("the builtin {b} ({})", if u { "unsigned" } else { "signed" }),
        })
    }

    fn convert(&self, b: &str, t: &Type, a: &Operand, v: &str) -> Res<String> {
        let sat = b.contains("_sat");
        let from_float = a.ty.scalar().is_float();
        let to_float = t.scalar().is_float();
        let to_w = bits(t);
        let lanes = t.lanes();
        let vt = |s: &str| if lanes > 1 { format!("vec{lanes}<{s}>") } else { s.to_string() };
        // The target's signedness is in the name: convert_uchar, convert_int...
        let target = b.trim_start_matches("convert_");
        let to_unsigned = target.starts_with('u');
        let from_w = bits(&a.ty);
        if (!from_float && from_w > 32) || (!to_float && to_w > 32) {
            bail!("64-bit integers");
        }
        let rounded = if b.contains("_rte") {
            format!("round({v})")
        } else if b.contains("_rtp") {
            format!("ceil({v})")
        } else if b.contains("_rtn") {
            format!("floor({v})")
        } else {
            v.to_string()
        };
        Ok(match (from_float, to_float) {
            (true, true) => v.to_string(),
            (false, true) => {
                // The source's signedness isn't in a convert's name; the
                // parameter's mangling was checked by the caller (unsigned).
                format!("{}({v})", vt("f32"))
            }
            (true, false) => {
                let (lo, hi): (i64, i64) = if to_unsigned { (0, mask(to_w) as i64) } else { (-(1i64 << (to_w - 1)), (1i64 << (to_w - 1)) - 1) };
                let x = if sat || to_w < 32 { format!("clamp({rounded}, {}({lo}.0), {}({hi}.0))", vt("f32"), vt("f32")) } else { rounded };
                let e = if to_unsigned { format!("{}({x})", vt("u32")) } else { format!("{}({}({x}))", vt("u32"), vt("i32")) };
                if to_w < 32 { format!("({e} & {})", self.splat(t, &format!("{}u", mask(to_w)))) } else { e }
            }
            (false, false) => {
                if to_w < 32 {
                    format!("({v} & {})", self.splat(t, &format!("{}u", mask(to_w))))
                } else if from_w < 32 {
                    // Widening: sign-extend unless the source is unsigned
                    // (the call's first parameter is mangled h/t).
                    format!("{}({})", vt("u32"), sext_signed(v, from_w, lanes))
                } else {
                    v.to_string()
                }
            }
        })
    }
}

fn targets(n: &Node, out: &mut BTreeSet<usize>) {
    match n {
        Node::Jump { to, .. } => {
            out.insert(*to);
        }
        Node::Seq(items) => items.iter().for_each(|i| targets(i, out)),
        Node::Block(l, b) | Node::Loop(l, b) => {
            let mut inner = BTreeSet::new();
            targets(b, &mut inner);
            inner.remove(l);
            out.extend(inner);
        }
        Node::If(_, a, b) | Node::Case(_, _, a, b) => {
            targets(a, out);
            targets(b, out);
        }
        _ => {}
    }
}

fn atomic_builtin(name: &str) -> bool {
    demangle(name).is_some_and(|(b, _)| (b.starts_with("atomic_") || b.starts_with("atom_")) && !b.starts_with("atomic_work_item_fence"))
}

fn name(b: Base) -> String {
    name_of(b)
}

fn name_of(b: Base) -> String {
    match b {
        Base::Block => "args".into(),
        Base::Buffer(k) => format!("buf{k}"),
        Base::LocalArg(k) => format!("loc{k}"),
        Base::Private(r) => format!("priv{r}"),
        Base::Global(k) => format!("glob{k}"),
    }
}

/// An Itanium-mangled OpenCL builtin's name and its parameters' type
/// letters (vectors as their element's): `_Z13get_global_idj` is
/// ("get_global_id", ['j']).
pub fn demangle(s: &str) -> Option<(String, Vec<char>)> {
    let rest = s.strip_prefix("_Z")?;
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    let len: usize = rest[..digits].parse().ok()?;
    let name = rest.get(digits..digits + len)?.to_string();
    let mut params = Vec::new();
    let mut p = rest[digits + len..].chars().peekable();
    while let Some(c) = p.next() {
        match c {
            'D' => {
                // Dv4_f: a vector; Dh: half.
                if p.next() == Some('v') {
                    while p.next().is_some_and(|c| c != '_') {}
                    if let Some(e) = p.next() {
                        params.push(e);
                    }
                } else {
                    params.push('h');
                }
            }
            // Qualifiers, address spaces (PU3AS1), substitutions (S_): not types.
            'P' | 'K' | 'V' | 'U' | 'R' => {
                if c == 'U' {
                    // U3AS1: a length-prefixed vendor qualifier.
                    let n: String = std::iter::from_fn(|| p.next_if(|c| c.is_ascii_digit())).collect();
                    for _ in 0..n.parse::<usize>().unwrap_or(0) {
                        p.next();
                    }
                }
            }
            'S' => {
                while p.next().is_some_and(|c| c != '_') {}
            }
            c => params.push(c),
        }
    }
    Some((name, params))
}

fn bits(t: &Type) -> u32 {
    match t.scalar() {
        Type::Int(b) => *b,
        Type::Ptr(_) => 32,
        Type::Half => 16,
        Type::Float => 32,
        Type::Double => 64,
        _ => 0,
    }
}

fn mask(b: u32) -> u32 {
    if b >= 32 { u32::MAX } else { (1u32 << b) - 1 }
}

fn sext(n: u64, b: u32) -> i64 {
    if b >= 64 || b == 0 { n as i64 } else { ((n << (64 - b)) as i64) >> (64 - b) }
}

/// A zero-extended b-bit value (u32) as a sign-extended i32.
fn sext_signed(v: &str, b: u32, lanes: u32) -> String {
    let (i, s) = if lanes > 1 { (format!("vec{lanes}<i32>"), format!("vec{lanes}<u32>({}u)", 32 - b)) } else { ("i32".into(), format!("{}u", 32 - b)) };
    if b >= 32 { format!("bitcast<{i}>({v})") } else { format!("(bitcast<{i}>({v} << {s}) >> {s})") }
}

/// sext_signed's result as u32 bits (for GEP indices).
fn sext_expr(v: &str, b: u32) -> String {
    format!("bitcast<u32>({})", sext_signed(v, b, 1))
}

fn float(x: f32) -> String {
    if x.is_finite() {
        let s = format!("{x:?}");
        if s.contains(['.', 'e']) { format!("{s}f") } else { format!("{s}.0f") }
    } else {
        format!("bitcast<f32>({}u)", x.to_bits())
    }
}

/// Functions generated code may call, by name.
const LIBRARY: &[(&str, &str)] = &[(
    "mul_hi_u32",
    "fn mul_hi_u32(a: u32, b: u32) -> u32 {
    let al = a & 0xffffu;
    let ah = a >> 16u;
    let bl = b & 0xffffu;
    let bh = b >> 16u;
    let lh = al * bh;
    let hl = ah * bl;
    let mid = ((al * bl) >> 16u) + (lh & 0xffffu) + (hl & 0xffffu);
    return ah * bh + (lh >> 16u) + (hl >> 16u) + (mid >> 16u);
}",
)];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencl::{build, test_clang};

    /// A kernel's WGSL, checked by naga as wgpu would.
    pub(crate) fn wgsl(src: &str, kernel: &str, launch: &Launch) -> String {
        let m = build(&test_clang(), src, "").unwrap();
        let w = compile(&m, kernel, launch).unwrap_or_else(|e| panic!("{kernel}: {e}"));
        let module = naga::front::wgsl::parse_str(&w).unwrap_or_else(|e| panic!("{}\n{w}", e.emit_to_string(&w)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}\n{w}", e.emit_to_string(&w)));
        w
    }

    const KERNELS: &str = r#"
__kernel void add(__global const float *a, __global const float *b, __global float *c, int n) {
    int i = get_global_id(0);
    if (i < n) c[i] = a[i] + b[i];
}
__kernel void reduce(__global const int *in, __global int *out, __local int *tmp) {
    int l = get_local_id(0);
    tmp[l] = in[get_global_id(0)];
    barrier(CLK_LOCAL_MEM_FENCE);
    for (int s = get_local_size(0) / 2; s > 0; s >>= 1) {
        if (l < s) tmp[l] += tmp[l + s];
        barrier(CLK_LOCAL_MEM_FENCE);
    }
    if (l == 0) atomic_add(out, tmp[0]);
}
__kernel void bytes(__global uchar *p, float4 v, __global float4 *q) {
    size_t i = get_global_id(0);
    p[i] = p[i] * 2 + 1;
    q[i] = q[i] * v + (float4)(sqrt(v.x), native_sin(v.y), fma(v.z, v.w, 1.0f), 0);
}
static void helper(__global float * restrict a, __global const float * restrict b, int i) { a[i] += b[i]; }
__kernel void restricted(__global float * restrict a, __global const float * restrict b) { helper(a, b, get_global_id(0)); }
__constant int table[4] = {3, 1, 4, 1};
typedef struct { int a; float b; char c; } params;
__kernel void misc(__global int *out, params p, __global short *s) {
    int i = get_global_id(0);
    int acc = 0;
    for (int k = 0; k < i % 7; k++) {
        if (k == 3) continue;
        acc += table[k & 3] * (k - p.a);
        if (acc > 100) break;
    }
    switch (i & 3) {
        case 0: acc += 1; break;
        case 1: acc *= 2; break;
        case 2: acc = -acc; break;
        default: acc = min(acc, (int)p.b);
    }
    out[i] = acc + p.c + convert_int(p.b * 2.5f) + (s[i] >> 2) + clz(i) + popcount(i) + abs(s[i]);
    s[i] = (short)(s[i] * 3);
}
"#;

    #[test]
    fn kernels_become_valid_wgsl() {
        let launch = Launch { local: [64, 1, 1], local_bytes: vec![(2, 256)] };
        for k in ["add", "reduce", "bytes", "misc", "restricted"] {
            wgsl(KERNELS, k, &launch);
        }
    }

    #[test]
    fn arguments_are_laid_out_as_opencl_does() {
        let m = build(&test_clang(), KERNELS, "").unwrap();
        let info = kernel_info(&m, "misc").unwrap();
        let h = HEADER_WORDS * 4;
        assert_eq!(info.args.iter().map(|a| (a.kind.clone(), a.offset)).collect::<Vec<_>>(), [
            (ArgKind::Buffer { constant: false }, h),
            (ArgKind::Value { size: 12 }, h + 4),
            (ArgKind::Buffer { constant: false }, h + 16),
        ]);
        assert_eq!(info.args[1].type_name, "params");
        assert_eq!(info.args[0].name, "out");
    }

    #[test]
    fn builtins_demangle() {
        assert_eq!(demangle("_Z13get_global_idj"), Some(("get_global_id".into(), vec!['j'])));
        assert_eq!(demangle("_Z3minDv4_iS_"), Some(("min".into(), vec!['i'])));
        assert_eq!(demangle("_Z10atomic_addPU3AS1Vii"), Some(("atomic_add".into(), vec!['i', 'i'])));
    }
}
