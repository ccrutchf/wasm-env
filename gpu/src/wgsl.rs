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

use crate::ir::{Const, Function, Inst, Module, Op, Operand, Res, Term, Type, Value, bail};
use crate::structure::{Node, structure};

/// The argument block's header, in words: the global offset (3), the
/// global size (3), the work dimension, and the group offset (3) of this
/// dispatch (a launch larger than one dispatch allows is several).
pub const HEADER_WORDS: u32 = 16;
/// What every pointer's value adds to its byte offset, so that only a null
/// pointer is 0 (a buffer's start is a pointer like any other, and kernels
/// compare pointers with NULL). A page, so alignments are kept.
const BIAS: u32 = 4096;
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
    /// OpenCL's qualifiers: the type's (CL_KERNEL_ARG_TYPE_CONST 1, RESTRICT 2,
    /// VOLATILE 4, PIPE 8) and the access (CL_KERNEL_ARG_ACCESS_*, or 0).
    pub type_qualifiers: u32,
    pub access: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KernelInfo {
    pub name: String,
    pub args: Vec<Arg>,
    /// The argument block's size in bytes.
    pub block_size: u32,
    /// `reqd_work_group_size`, if the kernel has one.
    pub required_local: Option<[u32; 3]>,
    /// The kernel's attributes, as clGetKernelInfo gives them:
    /// "reqd_work_group_size(X,Y,Z) vec_type_hint(T)" and the like.
    pub attributes: String,
    /// Bytes of `__local` variables the kernel declares (not its arguments).
    pub static_local: u32,
}

/// A kernel's arguments and their layout.
pub fn kernel_info(m: &Module, name: &str) -> Res<KernelInfo> {
    let f = m.function(name).filter(|f| f.kernel).ok_or_else(|| format!("no kernel {name}"))?;
    let types = f.metadata.get("kernel_arg_type").map(|&n| m.meta_strings(n)).unwrap_or_default();
    let names = f.metadata.get("kernel_arg_name").map(|&n| m.meta_strings(n)).unwrap_or_default();
    let quals = f.metadata.get("kernel_arg_type_qual").map(|&n| m.meta_strings(n)).unwrap_or_default();
    let access = f.metadata.get("kernel_arg_access_qual").map(|&n| m.meta_strings(n)).unwrap_or_default();
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
            type_qualifiers: quals.get(k).map_or(0, |q| {
                let has = |w: &str| q.split_whitespace().any(|x| x == w);
                has("const") as u32 | (has("restrict") as u32) << 1 | (has("volatile") as u32) << 2 | (has("pipe") as u32) << 3
            }),
            access: match access.get(k).map(String::as_str) {
                Some("read_only") => 0x11A0,
                Some("write_only") => 0x11A1,
                Some("read_write") => 0x11A2,
                Some("none") => 0x11A3,
                _ => 0,
            },
            name: names.get(k).cloned().unwrap_or_default(),
        });
        offset += size;
    }
    let required_local = f.metadata.get("reqd_work_group_size").map(|&n| m.meta_ints(n)).filter(|v| v.len() == 3).map(|v| [v[0] as u32, v[1] as u32, v[2] as u32]);
    let attributes = attributes(m, f, required_local);
    // (clang names a kernel's __local variables "kernel.variable".)
    let prefix = format!("{name}.");
    let static_local = m.globals.iter().filter(|g| g.space == 3 && g.name.starts_with(&prefix)).map(|g| m.layout(&g.ty).map_or(0, |(s, _)| s as u32)).sum();
    Ok(KernelInfo { name: name.to_string(), args, block_size: offset.next_multiple_of(4), required_local, attributes, static_local })
}

/// A byval argument's struct type: recorded by the parser from the
/// signature's `byval(T)`.
fn byval_type(_m: &Module, f: &Function, k: usize) -> Option<Type> {
    f.byval.get(&k).cloned()
}

/// What a device does that OpenCL doesn't allow, which kernels make up for
/// (`Gpu::open` finds out).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Quirks {
    /// Products just under the smallest normal number flushed to zero where
    /// rounding them (to the denormals' precision) gives that number: x86's
    /// flush to zero, which lavapipe has, takes a result as tiny after
    /// rounding it with an unbounded exponent.
    pub flushes_rounded_up_products: bool,
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

pub fn compile(m: &Module, name: &str, launch: &Launch, quirks: Quirks) -> Res<String> {
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
    let mut g = Gen { m, f, info: &info, launch, quirks, out: String::new(), bases: HashMap::new(), atomic: BTreeSet::new(), private: BTreeMap::new(), depth: 1, temp: 0, lets: BTreeSet::new(), vector_call: false, loop_exit: None, lane: None, needs: Default::default() };
    g.provenance()?;
    g.lets = local_values(f);
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
    quirks: Quirks,
    out: String,
    /// Each pointer value's base.
    bases: HashMap<Value, Base>,
    atomic: BTreeSet<Base>,
    /// Allocas: their size in words.
    private: BTreeMap<usize, u64>,
    depth: usize,
    temp: usize,
    /// Values used only in the block that defines them: `let`s there
    /// rather than variables (`local_values`).
    lets: BTreeSet<usize>,
    /// Whether the builtin being called is a vector form (OpenCL's select
    /// differs), when its lanes are done one at a time.
    vector_call: bool,
    /// The Block the next Loop is the whole of (see `node`).
    loop_exit: Option<usize>,
    /// Where a lane-at-a-time instruction's result goes (`wide`).
    lane: Option<String>,
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
            Value::Wgsl(s) => (3u8, s.clone()).hash(h),
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
                            // (A few words beyond, as drivers have: programs (the CTS
                            // among them) store a little past a private array.)
                            self.private.insert(r, (s * n).div_ceil(4) + 4);
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
                    Op::Call(name, args) if demangle(name).is_some_and(|(b, _)| b.starts_with("async_work_group")) => {
                        let b = self.base(&args[0])?;
                        if elem_size(name).is_none_or(|s| s % 4 != 0) && matches!(b, Base::Buffer(_) | Base::LocalArg(_) | Base::Global(_)) {
                            self.atomic.insert(b);
                        }
                    }
                    // vstoreN of elements narrower than a word: bytes
                    // other work-items write share its words.
                    Op::Call(name, args) if demangle(name).is_some_and(|(b, _)| b.starts_with("vstore")) && args.len() == 3 => {
                        let b = self.base(&args[2])?;
                        let (size, _) = self.m.layout(args[0].ty.scalar())?;
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
        // (With what they call: WGSL takes functions in any order.)
        let mut needed = self.needs.borrow().clone();
        loop {
            let more: BTreeSet<&str> = LIBRARY.iter().filter(|(n, _, _)| needed.contains(n)).flat_map(|(_, deps, _)| deps.iter().copied()).collect();
            if more.is_subset(&needed) {
                break;
            }
            needed.extend(more);
        }
        for (n, _, code) in LIBRARY {
            if needed.contains(n) {
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
                // (A null buffer is 0xffffffff in the block: the pointer 0.)
                (ArgKind::Buffer { .. }, _) => format!("select(args[{o}] + {BIAS}u, 0u, args[{o}] == 0xffffffffu)", o = a.offset / 4),
                (ArgKind::Local, _) => format!("{BIAS}u"),
                (_, Type::Ptr(0)) => format!("{}u", a.offset + BIAS),
                (_, t) => self.load_expr(Base::Block, &format!("{}u", a.offset + BIAS), t, 4)?,
            };
            writeln!(o, "    let a{k} = {v};").unwrap();
        }
        for (r, t) in self.f.values.iter().enumerate() {
            if self.lets.contains(&r) {
                continue;
            }
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
            "fn ld_{n}(p: u32) -> u32 {{\n    let i = (p - 4096u) >> 2u;\n    let s = (p & 3u) * 8u;\n    if (s == 0u) {{ return {}; }}\n    return ({} >> s) | ({} << (32u - s));\n}}",
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
        let whole = if atomic { format!("atomicStore(&{n}[(p - 4096u) >> 2u], v)") } else { format!("{n}[(p - 4096u) >> 2u] = v") };
        writeln!(
            o,
            "fn st_{n}(p: u32, v: u32, bytes: u32) {{\n    if ((p & 3u) == 0u && bytes == 4u) {{ {whole}; return; }}\n    for (var k = 0u; k < bytes; k++) {{\n        let q = p + k;\n        let i = (q - 4096u) >> 2u;\n        let s = (q & 3u) * 8u;\n        {set_byte}\n    }}\n}}"
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
            // 64-bit integers: (low word, high word); vectors of them, arrays.
            Type::Int(64) => "vec2<u32>".into(),
            Type::Vector(n, e) if **e == Type::Int(64) => format!("array<vec2<u32>, {n}>"),
            Type::Float => "f32".into(),
            Type::Vector(n @ 2..=4, e) => format!("vec{n}<{}>", self.wty(e)?),
            // Wider vectors (8 and 16 lanes): arrays, operated on lane by lane.
            Type::Vector(n @ (8 | 16), e) => format!("array<{}, {n}>", self.wty(e)?),
            t => bail!("values of type {t:?}"),
        }))
    }

    /// A value as a WGSL expression.
    fn val(&self, o: &Operand) -> Res<String> {
        match &o.v {
            Value::Local(n) => Ok(format!("v{n}")),
            Value::Arg(n) => Ok(format!("a{n}")),
            Value::Const(c) => self.constant(&o.ty, c),
            Value::Wgsl(s) => Ok(s.clone()),
        }
    }

    fn constant(&self, t: &Type, c: &Const) -> Res<String> {
        let t = self.m.resolve(t)?;
        Ok(match (c, t) {
            (Const::Int(n), Type::Int(1)) => (if n & 1 != 0 { "true" } else { "false" }).into(),
            (Const::Int(n), Type::Int(64)) => format!("vec2<u32>({}u, {}u)", *n as u32, (*n >> 32) as u32),
            (Const::Int(n), Type::Int(b)) => format!("{}u", n & mask(*b) as u64),
            (Const::Float(x), Type::Float) => float(*x as f32),
            (Const::Undef | Const::Zero | Const::Null, Type::Ptr(_)) => "0u".into(),
            (Const::Undef | Const::Zero | Const::Null, t) => format!("{}()", self.wty(t)?),
            (Const::Vector(items), Type::Vector(_, e)) => {
                let items: Vec<String> = items.iter().map(|i| self.constant(e, i)).collect::<Res<_>>()?;
                format!("{}({})", self.wty(t)?, items.join(", "))
            }
            (Const::Global(_), Type::Ptr(_)) => format!("{BIAS}u"),
            (Const::Gep(src, ops), Type::Ptr(_)) => {
                let base = self.val(&ops[0])?;
                let off = self.gep_offset(src, &ops[1..])?;
                format!("({base} + {off})")
            }
            // (A pointer as a 64-bit integer: its offset, widened.)
            (Const::Cast(_, o), Type::Int(64)) if bits(&o.ty) != 64 => format!("vec2<u32>({}, 0u)", self.val(o)?),
            (Const::Cast(_, o), Type::Int(b)) if bits(&o.ty) == 64 && *b < 64 => format!("{}.x", self.val(o)?),
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
            // (Pointers are 32-bit: the low word is the index that matters.)
            64 => format!("{v}.x"),
            b => sext_expr(&v, b),
        })
    }

    // ---- Structured code ----

    /// `stack`: the WGSL loops around, innermost last: (a Loop?, its label,
    /// the Block a Loop is the whole of, whose end a `break` reaches).
    fn node(&mut self, n: &Node, stack: &mut Vec<(bool, usize, Option<usize>)>) -> Res<()> {
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
                // A Block that is just a Loop: leaving the loop leaves it.
                let exit = if is_loop { self.loop_exit.take() } else { None };
                if !is_loop && matches!(**body, Node::Loop(..)) {
                    self.loop_exit = Some(*label);
                }
                stack.push((is_loop, *label, exit));
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
                    let Some(&(parent_loop, parent, _)) = stack.last() else { bail!("a jump out of the function") };
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
                let test = if bits(&v.ty) == 64 { format!("all({x} == {k})") } else { format!("{x} == {k}") };
                self.line(&format!("if ({test}) {{"));
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
                    Some(&(true, l, _)) if l == *to => self.line("continue;"),
                    Some(&(true, _, Some(e))) if e == *to => self.line("break;"),
                    Some(&(false, l, _)) if l == *to => self.line("break;"),
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

    fn need(&self, n: &'static str) {
        self.needs.borrow_mut().insert(n);
    }

    fn set(&mut self, i: &Inst, e: String) {
        if let Some(t) = self.lane.clone() {
            self.line(&format!("{t} = {e};"));
        } else if let Some(r) = i.result {
            let keyword = if self.lets.contains(&r) { "let " } else { "" };
            self.line(&format!("{keyword}v{r} = {e};"));
        }
    }

    /// Whether an instruction works on vectors wider than WGSL's (as arrays),
    /// so it's done a lane at a time.
    fn wide(i: &Inst) -> Option<u32> {
        // (Vectors of 64-bit integers are arrays of vec2<u32> at any width.)
        let wide = |t: &Type| matches!(t, Type::Vector(n, e) if *n > 4 || **e == Type::Int(64));
        let lanes = |t: &Type| t.lanes();
        match &i.op {
            Op::Bin(..) | Op::FNeg(_) | Op::Select(..) | Op::Freeze(_) | Op::Cast(..) if wide(&i.ty) => Some(lanes(&i.ty)),
            Op::Cast(_, a) if wide(&a.ty) && a.ty.lanes() == i.ty.lanes() => Some(lanes(&i.ty)),
            Op::ICmp(_, a, _) | Op::FCmp(_, a, _) if wide(&a.ty) => Some(lanes(&a.ty)),
            Op::Call(name, _) if i.ty.lanes() > 1 && demangle(name).is_some_and(|(b, _)| SCALAR_ONLY.contains(&b.as_str())) => Some(lanes(&i.ty)),
            Op::Call(name, args) if (wide(&i.ty) || (i.ty.lanes() > 1 && args.iter().any(|a| wide(&a.ty) && a.ty.lanes() == i.ty.lanes()))) && !name.starts_with("llvm.mem") && !demangle(name).is_some_and(|(b, _)| b.starts_with("vload") || b.starts_with("shuffle")) => {
                let _ = args;
                Some(lanes(&i.ty))
            }
            _ => None,
        }
    }

    fn inst(&mut self, i: &Inst) -> Res<()> {
        if let (Some(n), Some(r)) = (Self::wide(i), i.result) {
            // Lane by lane: the instruction on each lane's scalars.
            for k in 0..n {
                let lane = |o: &Operand, g: &Self| -> Res<Operand> {
                    Ok(if matches!(o.ty, Type::Vector(m, _) if m == n) { Operand { ty: o.ty.scalar().clone(), v: Value::Wgsl(format!("{}[{k}]", g.val(o)?)) } } else { o.clone() })
                };
                let op = match &i.op {
                    Op::Bin(s, a, b) => Op::Bin(s.clone(), lane(a, self)?, lane(b, self)?),
                    Op::FNeg(a) => Op::FNeg(lane(a, self)?),
                    Op::Freeze(a) => Op::Freeze(lane(a, self)?),
                    Op::Select(c, a, b) => Op::Select(lane(c, self)?, lane(a, self)?, lane(b, self)?),
                    Op::Cast(s, a) => {
                        if a.ty.lanes() != n {
                            bail!("a {s} from {:?} to {:?}", a.ty, i.ty);
                        }
                        Op::Cast(s.clone(), lane(a, self)?)
                    }
                    Op::ICmp(p, a, b) => Op::ICmp(p.clone(), lane(a, self)?, lane(b, self)?),
                    Op::FCmp(p, a, b) => Op::FCmp(p.clone(), lane(a, self)?, lane(b, self)?),
                    Op::Call(name, args) => Op::Call(name.clone(), args.iter().map(|a| lane(a, self)).collect::<Res<_>>()?),
                    _ => unreachable!(),
                };
                let scalar = Inst { result: i.result, ty: i.ty.scalar().clone(), op };
                self.lane = Some(format!("v{r}[{k}]"));
                let done = self.inst(&scalar);
                self.lane = None;
                done?;
            }
            return Ok(());
        }
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
            Op::Alloca(..) => self.set(i, format!("{BIAS}u")),
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
                if let Value::Const(Const::Int(k)) = k.v {
                    // A constant lane: the vector built anew, in one expression.
                    let (vv, ev) = (self.val(v)?, self.val(e)?);
                    let lanes: Vec<String> = (0..v.ty.lanes() as u64).map(|j| if j == k { ev.clone() } else { format!("{vv}[{j}]") }).collect();
                    let ty = self.wty(&i.ty)?;
                    self.set(i, format!("{ty}({})", lanes.join(", ")));
                    return Ok(());
                }
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
            _ if bits(&k.ty) == 64 => format!("{}.x", self.val(k)?),
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
            if op == "fmul" && self.quirks.flushes_rounded_up_products && *t.scalar() == Type::Float {
                self.need("fmul_ftz");
                let lanes = t.lanes();
                if lanes == 1 {
                    return Ok(format!("fmul_ftz({x}, {y})"));
                }
                let l: Vec<String> = (0..lanes).map(|k| format!("fmul_ftz(({x})[{k}], ({y})[{k}])")).collect();
                return Ok(format!("vec{lanes}<f32>({})", l.join(", ")));
            }
            return Ok(format!("{x} {o} {y}"));
        }
        let w = bits(t);
        if w == 1 {
            // i1 as bool (`&` and `|`: WGSL's bool vectors have no `&&`, `||`).
            return Ok(match op {
                "and" => format!("({x} & {y})"),
                "or" => format!("({x} | {y})"),
                "xor" | "add" | "sub" => format!("{x} != {y}"),
                "mul" => format!("({x} & {y})"),
                o => bail!("{o} on i1"),
            });
        }
        if w == 64 {
            let call = |n: &'static str, y: &str| {
                self.need(n);
                format!("{n}({x}, {y})")
            };
            return Ok(match op {
                "add" => call("add64", &y),
                "sub" => call("sub64", &y),
                "mul" => call("mul64", &y),
                "udiv" => call("udiv64", &y),
                "urem" => call("urem64", &y),
                "sdiv" => call("sdiv64", &y),
                "srem" => call("srem64", &y),
                "shl" => call("shl64", &format!("{y}.x")),
                "lshr" => call("lshr64", &format!("{y}.x")),
                "ashr" => call("ashr64", &format!("{y}.x")),
                "and" => format!("{x} & {y}"),
                "or" => format!("{x} | {y}"),
                "xor" => format!("{x} ^ {y}"),
                o => bail!("the operation {o}"),
            });
        }
        if w > 32 {
            bail!("integers of {w} bits");
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
        if w == 64 {
            let (lt, x, y) = if p.starts_with('s') { ("slt64", x, y) } else { ("ult64", x, y) };
            self.need(lt);
            return Ok(match p {
                "eq" => format!("all({x} == {y})"),
                "ne" => format!("any({x} != {y})"),
                "ult" | "slt" => format!("{lt}({x}, {y})"),
                "ugt" | "sgt" => format!("{lt}({y}, {x})"),
                "ule" | "sle" => format!("!{lt}({y}, {x})"),
                "uge" | "sge" => format!("!{lt}({x}, {y})"),
                p => bail!("icmp {p}"),
            });
        }
        if w > 32 && !matches!(a.ty.scalar(), Type::Ptr(_)) {
            bail!("integers of {w} bits");
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
        // NaN by its bits (a driver may take `x == x` to be true), and `&`,
        // `|` rather than `&&`, `||`, which WGSL's bool vectors don't have.
        let lanes = a.ty.lanes();
        let u = if lanes > 1 { format!("vec{lanes}<u32>") } else { "u32".into() };
        let s = |n: u32| if lanes > 1 { format!("vec{lanes}<u32>({n}u)") } else { format!("{n}u") };
        let number = |v: &str| format!("((bitcast<{u}>({v}) & {}) <= {})", s(0x7fff_ffff), s(0x7f80_0000));
        let ord = format!("({} & {})", number(&x), number(&y));
        let c = |o: &str| format!("({x} {o} {y})");
        Ok(match p {
            "oeq" => format!("({} & {ord})", c("==")),
            "one" => format!("({} & {ord})", c("!=")),
            "olt" => format!("({} & {ord})", c("<")),
            "ole" => format!("({} & {ord})", c("<=")),
            "ogt" => format!("({} & {ord})", c(">")),
            "oge" => format!("({} & {ord})", c(">=")),
            "ord" => ord,
            "uno" => format!("!{ord}"),
            "ueq" => format!("({} | !{ord})", c("==")),
            "une" => format!("({} | !{ord})", c("!=")),
            "ult" => format!("({} | !{ord})", c("<")),
            "ule" => format!("({} | !{ord})", c("<=")),
            "ugt" => format!("({} | !{ord})", c(">")),
            "uge" => format!("({} | !{ord})", c(">=")),
            "true" => if lanes > 1 { format!("vec{lanes}<bool>(true)") } else { "true".into() },
            "false" => if lanes > 1 { format!("vec{lanes}<bool>(false)") } else { "false".into() },
            p => bail!("fcmp {p}"),
        })
    }

    fn cast(&self, op: &str, a: &Operand, to: &Type) -> Res<String> {
        let x = self.val(a)?;
        let (from_w, to_w) = (bits(&a.ty), bits(to));
        let lanes = to.lanes();
        let vt = |s: &str| if lanes > 1 { format!("vec{lanes}<{s}>") } else { s.to_string() };
        let int64 = |t: &Type| *t == Type::Int(64);
        if int64(to) || int64(&a.ty) {
            let call = |n: &'static str| {
                self.need(n);
                format!("{n}({x})")
            };
            return Ok(match op {
                "zext" if from_w == 1 => format!("vec2<u32>(select(0u, 1u, {x}), 0u)"),
                "zext" | "ptrtoint" => format!("vec2<u32>({x}, 0u)"),
                "sext" if from_w == 1 => format!("select(vec2<u32>(0u, 0u), vec2<u32>(0xffffffffu, 0xffffffffu), {x})"),
                "sext" => {
                    let s = sext_signed(&x, from_w, 1);
                    format!("vec2<u32>(bitcast<u32>({s}), select(0u, 0xffffffffu, {s} < 0i))")
                }
                "trunc" if to_w == 1 => format!("(({x}.x & 1u) != 0u)"),
                "trunc" if to_w < 32 => format!("({x}.x & {}u)", mask(to_w)),
                "trunc" | "inttoptr" => format!("{x}.x"),
                "fptoui" => call("f32tou64"),
                "fptosi" => call("f32tos64"),
                "uitofp" => call("u64tof32"),
                "sitofp" => call("s64tof32"),
                "bitcast" => match (&a.ty, to) {
                    (Type::Int(64), Type::Int(64)) => x,
                    // <2 x i32> is a vec2<u32> too.
                    (Type::Vector(2, e), _) | (_, Type::Vector(2, e)) if **e == Type::Int(32) => x,
                    (Type::Vector(2, e), _) if **e == Type::Float => format!("bitcast<vec2<u32>>({x})"),
                    (_, Type::Vector(2, e)) if **e == Type::Float => format!("bitcast<vec2<f32>>({x})"),
                    (f, t) => bail!("a bitcast from {f:?} to {t:?}"),
                },
                op => bail!("{op} from {:?} to {to:?}", a.ty),
            });
        }
        if (from_w > 32 && !a.ty.scalar().is_float() && !matches!(a.ty.scalar(), Type::Ptr(_))) || (to_w > 32 && !to.scalar().is_float() && !matches!(to.scalar(), Type::Ptr(_))) {
            bail!("integers of {} bits", from_w.max(to_w));
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
            // <N x i1> to and from an N-bit mask, lane k in bit k.
            "bitcast" if a.ty.lanes() > 1 && from_w == 1 && to.lanes() == 1 => {
                let n = a.ty.lanes();
                let weights: Vec<String> = (0..n).map(|k| format!("{}u", 1u32 << k)).collect();
                format!("dot(select(vec{n}<u32>(0u), vec{n}<u32>({}), {x}), vec{n}<u32>(1u))", weights.join(", "))
            }
            "bitcast" if to.lanes() > 1 && to_w == 1 && a.ty.lanes() == 1 => {
                let n = to.lanes();
                let weights: Vec<String> = (0..n).map(|k| format!("{}u", 1u32 << k)).collect();
                format!("(vec{n}<u32>({x}) & vec{n}<u32>({})) != vec{n}<u32>(0u)", weights.join(", "))
            }
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
            if self.atomic.contains(&b) { format!("atomicLoad(&{n}[(({p}) - 4096u) >> 2u])") } else { format!("{n}[(({p}) - 4096u) >> 2u]") }
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
                if *t == Type::Int(64) {
                    let word = |q: String| if align.is_multiple_of(4) { self.word_at(b, &q, 4) } else { format!("ld_{}({q})", name(b)) };
                    return Ok(format!("vec2<u32>({}, {})", word(p.to_string()), word(format!("({p} + 4u)"))));
                }
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
                // Narrow lanes filling whole aligned words: packed into words,
                // stored a word at a time.
                if s < 4 && (*n as u64 * s).is_multiple_of(4) && align >= 4 && !e.is_float() {
                    let per = (4 / s) as u32;
                    for w in 0..*n / per {
                        let word: Vec<String> = (0..per).map(|k| format!("({v}[{}] << {}u)", w * per + k, k as u64 * s * 8)).collect();
                        self.store(b, &format!("({p} + {}u)", w * 4), &Type::Int(32), &format!("({})", word.join(" | ")), 4)?;
                    }
                    return Ok(());
                }
                for k in 0..*n {
                    self.store(b, &format!("({p} + {}u)", k as u64 * s), e, &format!("{v}[{k}]"), if align as u64 >= s { s as u32 } else { align })?;
                }
            }
            Type::Int(64) => {
                let a = align.min(4);
                self.store(b, p, &Type::Int(32), &format!("{v}.x"), a)?;
                self.store(b, &format!("({p} + 4u)"), &Type::Int(32), &format!("{v}.y"), a)?;
            }
            _ => {
                let (s, _) = self.m.layout(&t)?;
                let w = self.to_word(v, &t)?;
                let n = name(b);
                let atomic = self.atomic.contains(&b);
                if s == 4 && align.is_multiple_of(4) {
                    if atomic {
                        self.line(&format!("atomicStore(&{n}[(({p}) - 4096u) >> 2u], {w});"));
                    } else {
                        self.line(&format!("{n}[(({p}) - 4096u) >> 2u] = {w};"));
                    }
                } else if s == 1 || (s == 2 && align >= 2) {
                    // Within one word: its bits replaced, inline.
                    let m = if s == 1 { "0xffu" } else { "0xffffu" };
                    self.line("{");
                    self.line(&format!("    let q = {p};"));
                    self.line("    let s = (q & 3u) * 8u;");
                    if atomic {
                        self.line(&format!("    atomicAnd(&{n}[(q - 4096u) >> 2u], ~({m} << s));"));
                        self.line(&format!("    atomicOr(&{n}[(q - 4096u) >> 2u], (({w}) & {m}) << s);"));
                    } else {
                        self.line(&format!("    {n}[(q - 4096u) >> 2u] = ({n}[(q - 4096u) >> 2u] & ~({m} << s)) | ((({w}) & {m}) << s);"));
                    }
                    self.line("}");
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
        self.line(&format!("    let r = atomicCompareExchangeWeak(&{n}[(({p}) - 4096u) >> 2u], {cmp}, {new});"));
        self.line(&format!("    if (r.exchanged || r.old_value != {cmp}) {{ old{t} = r.old_value; break; }}"));
        self.line("}");
        format!("old{t}")
    }

    fn atomic_rmw(&mut self, b: Base, kind: &str, p: &str, v: &str, t: &Type) -> Res<String> {
        let n = name(b);
        let at = format!("&{n}[(({p}) - 4096u) >> 2u]");
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
        // (libclc's clspv build leaves a few of its own to the target:
        // __clc_mul_hi and the like, the builtins of the same name.)
        let base = base.strip_prefix("__clc_").map_or(base.clone(), str::to_string);
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
            "wait_group_events" => {
                self.line("workgroupBarrier();");
                self.line("storageBarrier();");
                return Ok(());
            }
            "async_work_group_copy" | "async_work_group_strided_copy" => {
                // The work-group's items share the copy, each taking every
                // (group size)th element; words at a time.
                let size = elem_size(name).ok_or_else(|| format!("the element type of {name}"))?;
                let (db, sb) = (self.base(&args[0])?, self.base(&args[1])?);
                let stride = if base == "async_work_group_strided_copy" { vals[3].clone() } else { "1u".into() };
                // Strided: the global side.
                let (ds, ss) = if matches!(args[0].ty, Type::Ptr(3)) { ("1u".to_string(), stride) } else { (stride, "1u".to_string()) };
                let [x, y, z] = self.launch.local;
                let (dn, sn) = (name_of(db), name_of(sb));
                self.line(&format!("for (var e = lid.x + lid.y * {x}u + lid.z * {}u; e < {}; e += {}u) {{", x * y, vals[2], x * y * z));
                if size % 4 == 0 {
                    self.line(&format!("    for (var w = 0u; w < {}u; w++) {{", size / 4));
                    self.line(&format!("        st_{dn}({} + e * {ds} * {size}u + w * 4u, ld_{sn}({} + e * {ss} * {size}u + w * 4u), 4u);", vals[0], vals[1]));
                    self.line("    }");
                } else {
                    self.line(&format!("    for (var w = 0u; w < {size}u; w++) {{"));
                    self.line(&format!("        st_{dn}({} + e * {ds} * {size}u + w, ld_{sn}({} + e * {ss} * {size}u + w) & 0xffu, 1u);", vals[0], vals[1]));
                    self.line("    }");
                }
                self.line("}");
                "0u".to_string()
            }
            "barrier" | "work_group_barrier" => {
                self.line("workgroupBarrier();");
                if !matches!(args.first().map(|a| &a.v), Some(Value::Const(Const::Int(1)))) {
                    self.line("storageBarrier();");
                }
                return Ok(());
            }
            "mem_fence" | "read_mem_fence" | "write_mem_fence" | "prefetch" => return Ok(()),
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
            b => {
                // Scalar arguments to vector forms (fmax(float4, float), step(float,
                // float4), clamp(int4, int, int)): splatted, as WGSL wants.
                let lanes = i.ty.lanes();
                let vals: Vec<String> = if lanes > 1 && lanes <= 4 && SPLATTED.contains(&b) {
                    args.iter().zip(&vals).map(|(a, v)| Ok(if a.ty.lanes() == 1 { format!("vec{lanes}<{}>({v})", self.wty(a.ty.scalar())?) } else { v.clone() })).collect::<Res<_>>()?
                } else {
                    vals
                };
                self.vector_call = name.contains("Dv");
                self.math(b, i, args, &vals, unsigned)?
            }
        };
        self.set(i, e);
        Ok(())
    }

    fn intrinsic(&mut self, i: &Inst, name: &str, args: &[Operand], v: &[String]) -> Res<()> {
        let base = name.split('.').next().unwrap_or(name);
        let t = &i.ty;
        if bits(t) == 64 && !t.scalar().is_float() {
            let lt = |s: bool| {
                let n = if s { "slt64" } else { "ult64" };
                self.need(n);
                n
            };
            let call = |n: &'static str| {
                self.need(n);
                format!("{n}({})", v[0])
            };
            let e = match base {
                "smin" | "umin" => format!("select({a}, {b}, {}({b}, {a}))", lt(base == "smin"), a = v[0], b = v[1]),
                "smax" | "umax" => format!("select({a}, {b}, {}({a}, {b}))", lt(base == "smax"), a = v[0], b = v[1]),
                "abs" => call("abs64"),
                "ctpop" => call("popcount64"),
                "ctlz" => call("clz64"),
                "cttz" => call("ctz64"),
                n => bail!("the intrinsic llvm.{n} on 64-bit integers"),
            };
            self.set(i, e);
            return Ok(());
        }
        let e = match base {
            "lifetime" | "assume" | "experimental" | "dbg" | "invariant" => return Ok(()),
            "vector" if name.starts_with("vector.reduce.") => {
                // A vector folded to a scalar, lane by lane (the same
                // operations as on scalars, for the element type).
                let op = name.split('.').nth(2).unwrap_or("");
                let a = args.last().ok_or("a reduction without a vector")?;
                let et = a.ty.scalar().clone();
                let lane = |k: u32| Operand { ty: et.clone(), v: Value::Wgsl(format!("{}[{k}]", v[args.len() - 1])) };
                let mut acc = lane(0);
                for k in 1..a.ty.lanes() {
                    let e = match op {
                        "add" | "mul" | "and" | "or" | "xor" => self.binary(op, &acc, &lane(k))?,
                        "fadd" | "fmul" => self.binary(op, &acc, &lane(k))?,
                        "smax" | "smin" | "umax" | "umin" => {
                            let c = self.icmp(&format!("{}{}", &op[..1], if op.ends_with("max") { "gt" } else { "lt" }), &acc, &lane(k))?;
                            format!("select({}, {}, {c})", self.val(&lane(k))?, self.val(&acc)?)
                        }
                        "fmax" => min_max_num("max", &self.val(&acc)?, &self.val(&lane(k))?, 1),
                        "fmaximum" => format!("max({}, {})", self.val(&acc)?, self.val(&lane(k))?),
                        "fmin" => min_max_num("min", &self.val(&acc)?, &self.val(&lane(k))?, 1),
                        "fminimum" => format!("min({}, {})", self.val(&acc)?, self.val(&lane(k))?),
                        o => bail!("the reduction {o}"),
                    };
                    acc = Operand { ty: et.clone(), v: Value::Wgsl(format!("({e})")) };
                }
                // (fadd and fmul start from their first operand too.)
                let e = if (op == "fadd" || op == "fmul") && args.len() == 2 { self.binary(op, &args[0], &acc)? } else { self.val(&acc)? };
                self.set(i, e);
                return Ok(());
            }
            "fmuladd" | "fma" => format!("fma({}, {}, {})", v[0], v[1], v[2]),
            "fabs" => format!("abs({})", v[0]),
            "sqrt" | "floor" | "ceil" | "trunc" | "exp2" | "log2" | "exp" | "log" | "sin" | "cos" | "pow" => format!("{base}({})", v.join(", ")),
            "rint" | "nearbyint" | "roundeven" => format!("round({})", v[0]),
            "round" => format!("trunc({x} + sign({x}) * 0.5)", x = v[0]),
            "minnum" | "maxnum" => min_max_num(&base[..3], &v[0], &v[1], t.lanes()),
            "minimum" => format!("min({}, {})", v[0], v[1]),
            "maximum" => format!("max({}, {})", v[0], v[1]),
            "sadd" | "ssub" | "uadd" | "usub" if name.contains(".sat.") => self.saturating(t, base.ends_with("add"), base.starts_with('u'), &v[0], &v[1]),
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
                let len = if bits(&args[2].ty) == 64 { format!("{}.x", self.val(&args[2])?) } else { self.val(&args[2])? };
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

    /// add_sat and sub_sat (and llvm's sadd.sat etc.) on integers of up
    /// to 32 bits.
    fn saturating(&self, t: &Type, add: bool, unsigned: bool, a: &str, b: &str) -> String {
        let w = bits(t);
        let n = t.lanes();
        let s = |x: &str| self.splat(t, x);
        let op = if add { "+" } else { "-" };
        if unsigned {
            return if !add {
                format!("select({a} - {b}, {}, {b} > {a})", s("0u"))
            } else if w < 32 {
                format!("min({a} + {b}, {})", s(&format!("{}u", mask(w))))
            } else {
                format!("select({a} + {b}, {}, {a} + {b} < {a})", s("0xffffffffu"))
            };
        }
        let (x, y) = (sext_signed(a, w, n), sext_signed(b, w, n));
        let it = if n > 1 { format!("vec{n}<i32>") } else { "i32".into() };
        let e = if w < 32 {
            let (lo, hi) = (-(1i64 << (w - 1)), (1i64 << (w - 1)) - 1);
            format!("clamp({x} {op} {y}, {it}({lo}), {it}({hi}))")
        } else {
            // Overflow when the result's sign differs from a's, and b's
            // sign agrees with it (or, for subtraction, differs).
            let r = format!("({x} {op} {y})");
            let yy = if add { y.clone() } else { format!("(~{y})") };
            let over = format!("(({x} ^ {r}) & ({yy} ^ {r})) < {it}(0)");
            let sat = format!("select({it}(2147483647), {it}(-2147483647 - 1), {x} < {it}(0))");
            format!("select({r}, {sat}, {over})")
        };
        self.wrap_signed(t, e)
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
        let b = b.trim_start_matches("fast_");
        // A vector's length without overflow or underflow: scaled by its
        // largest component.
        let biggest = |x: &str, n: u32| (1..n).fold(format!("abs({x})[0]"), |m, k| format!("max({m}, abs({x})[{k}])"));
        let length = |x: &str, n: u32| {
            let m = biggest(x, n);
            // (Zero for zero; infinite if a component is.)
            format!("select(select(0.0, ({m}) * length({x} / ({m})), ({m}) > 0.0), ({m}), ({m}) > 3.4028235e38)")
        };
        let f1 = |f: &str| format!("{f}({})", v[0]);
        Ok(match b {
            "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sinh" | "cosh" | "tanh" | "asinh" | "acosh" | "atanh" | "exp" | "exp2" | "log" | "log2"
            | "floor" | "ceil" | "trunc" | "sign" | "degrees" | "radians" | "fract"
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
            "fmin" if float => min_max_num("min", &v[0], &v[1], t.lanes()),
            "fmax" if float => min_max_num("max", &v[0], &v[1], t.lanes()),
            "min" if float => format!("min({}, {})", v[0], v[1]),
            "max" if float => format!("max({}, {})", v[0], v[1]),
            "fmod" => format!("({} % {})", v[0], v[1]),
            "fdim" => format!("max({} - {}, {}(0.0))", v[0], v[1], self.wty(t)?),
            "fma" | "mad" => format!("fma({}, {}, {})", v[0], v[1], v[2]),
            // (OpenCL's definition, which rounds as the CTS expects.)
            "mix" => format!("({x} + ({y} - {x}) * {a})", x = v[0], y = v[1], a = v[2]),
            "clamp" if float => format!("clamp({}, {}, {})", v[0], v[1], v[2]),
            "step" => format!("step({}, {})", v[0], v[1]),
            "smoothstep" => format!("smoothstep({}, {}, {})", v[0], v[1], v[2]),
            "dot" if args[0].ty.lanes() == 1 => format!("({} * {})", v[0], v[1]),
            "dot" => format!("dot({}, {})", v[0], v[1]),
            // (float4: the cross product of xyz, w 0.)
            "cross" if args[0].ty.lanes() == 4 => format!("vec4<f32>(cross({}.xyz, {}.xyz), 0.0)", v[0], v[1]),
            "cross" => format!("cross({}, {})", v[0], v[1]),
            "length" if args[0].ty.lanes() > 1 => length(&v[0], args[0].ty.lanes()),
            "length" => format!("abs({})", v[0]),
            "distance" if args[0].ty.lanes() > 1 => length(&format!("({} - {})", v[0], v[1]), args[0].ty.lanes()),
            "distance" => format!("abs({} - {})", v[0], v[1]),
            "normalize" if args[0].ty.lanes() > 1 => {
                let m = biggest(&v[0], args[0].ty.lanes());
                format!("select({x}, normalize({x} / ({m})), ({m}) > 0.0)", x = v[0])
            }
            "normalize" => format!("sign({})", v[0]),
            "shuffle" | "shuffle2" => {
                let (n, m) = (args[0].ty.lanes(), t.lanes());
                let mask = &args[args.len() - 1];
                let mv = &v[args.len() - 1];
                let lanes: Vec<String> = (0..m)
                    .map(|k| {
                        let i = if bits(&mask.ty) == 64 { format!("{mv}[{k}].x") } else { format!("{mv}[{k}]") };
                        if b == "shuffle" {
                            format!("{}[{i} % {n}u]", v[0])
                        } else {
                            let i = format!("({i} % {}u)", 2 * n);
                            format!("select({x}[min({i}, {}u)], {y}[max({i}, {n}u) - {n}u], {i} >= {n}u)", n - 1, x = v[0], y = v[1])
                        }
                    })
                    .collect();
                if m == 1 { lanes[0].clone() } else { format!("{}({})", self.wty(t)?, lanes.join(", ")) }
            }
            "nextafter" => {
                let (x, y) = (&v[0], &v[1]);
                let bx = format!("bitcast<u32>({x})");
                let step = format!("select({bx} - 1u, {bx} + 1u, ({y} > {x}) == ({x} > 0.0))");
                let near = format!("select(select(bitcast<f32>({step}), bitcast<f32>(select(1u, 0x80000001u, {y} < 0.0)), {x} == 0.0), {y}, {x} == {y})");
                // (A NaN either side gives a NaN.)
                format!("select({near}, {x} + {y}, {} | {})", is_nan(x, 1), is_nan(y, 1))
            }
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
                    _ => format!("(({bitsx} & {}) < {}) & (({bitsx} & {}) >= {})", s(0x7fffffff), s(0x7f800000), s(0x7fffffff), s(0x00800000)),
                };
                // Scalars: 1 or 0; vectors: -1 (all bits) or 0.
                let yes = if lanes > 1 || self.vector_call { self.splat(t, &format!("{}u", mask(bits(t)))) } else { "1u".into() };
                format!("select({}, {yes}, {cond})", self.splat(t, "0u"))
            }
            "isequal" | "isnotequal" | "isgreater" | "isgreaterequal" | "isless" | "islessequal" | "islessgreater" | "isordered" | "isunordered" => {
                // As fcmp's predicates, NaN and all.
                let p = match b {
                    "isequal" => "oeq",
                    "isnotequal" => "une",
                    "isgreater" => "ogt",
                    "isgreaterequal" => "oge",
                    "isless" => "olt",
                    "islessequal" => "ole",
                    "islessgreater" => "one",
                    "isordered" => "ord",
                    _ => "uno",
                };
                let cond = self.fcmp(p, &args[0], &args[1])?;
                let yes = if t.lanes() > 1 || self.vector_call { self.splat(t, &format!("{}u", mask(bits(t)))) } else { "1u".into() };
                format!("select({}, {yes}, {cond})", self.splat(t, "0u"))
            }
            "select" => {
                // OpenCL's select(a, b, c): b where c is nonzero, or for vectors
                // where its top bit is set (lane by lane too: `vector_call`).
                let c = &args[2];
                let w = bits(c.ty.scalar());
                let cond = match (self.vector_call, w) {
                    (true, 64) => format!("(({}.y >> 31u) != 0u)", v[2]),
                    (false, 64) => format!("any({} != vec2<u32>(0u, 0u))", v[2]),
                    (true, w) => format!("(({} >> {}) & {}) != {}", v[2], self.splat(&c.ty, &format!("{}u", w - 1)), self.splat(&c.ty, "1u"), self.splat(&c.ty, "0u")),
                    (false, _) => format!("{} != 0u", v[2]),
                };
                format!("select({}, {}, {cond})", v[0], v[1])
            }
            "bitselect" => {
                // (a & ~c) | (b & c), on the bits.
                let t0 = &args[0].ty;
                if t0.scalar().is_float() {
                    let lanes = t0.lanes();
                    let u = if lanes > 1 { format!("vec{lanes}<u32>") } else { "u32".into() };
                    format!("bitcast<{}>((bitcast<{u}>({a}) & ~bitcast<{u}>({c})) | (bitcast<{u}>({b}) & bitcast<{u}>({c})))", self.wty(t0)?, a = v[0], b = v[1], c = v[2])
                } else {
                    let e = format!("(({a} & ~{c}) | ({b} & {c}))", a = v[0], b = v[1], c = v[2]);
                    let w = bits(t0.scalar());
                    if w < 32 { format!("({e} & {})", self.splat(t0, &format!("{}u", mask(w)))) } else { e }
                }
            }
            "any" | "all" => {
                // Of the top bits (any(x < 0) for a scalar).
                let a = &args[0];
                let w = bits(a.ty.scalar());
                let top = |x: &str| if w == 64 { format!("(({x}.y >> 31u) != 0u)") } else { format!("((({x} >> {}u) & 1u) != 0u)", w - 1) };
                let lanes: Vec<String> = if a.ty.lanes() > 1 { (0..a.ty.lanes()).map(|k| top(&format!("{}[{k}]", v[0]))).collect() } else { vec![top(&v[0])] };
                let joined = lanes.join(if b == "any" { " || " } else { " && " });
                format!("select(0u, 1u, {joined})")
            }
            // Integers.
            "min" | "max" | "clamp" | "abs" | "abs_diff" | "add_sat" | "sub_sat" | "hadd" | "rhadd" | "mad24" | "mul24" | "mad_hi" | "mul_hi" | "rotate" | "clz" | "ctz"
            | "popcount" | "upsample" | "mad_sat" => self.integer(b, t, args, v, unsigned)?,
            b if b.starts_with("convert_") => self.convert(b, t, &args[0], &v[0], unsigned)?,
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
        if w == 64 {
            let lt = if unsigned { "ult64" } else { "slt64" };
            let call = |n: &'static str| {
                self.need(n);
                format!("{n}({})", v[0])
            };
            self.need(lt);
            let min = |a: &str, b: &str| format!("select({a}, {b}, {lt}({b}, {a}))");
            let max = |a: &str, b: &str| format!("select({a}, {b}, {lt}({a}, {b}))");
            return Ok(match (b, unsigned) {
                ("min", _) => min(&v[0], &v[1]),
                ("max", _) => max(&v[0], &v[1]),
                ("clamp", _) => min(&max(&v[0], &v[1]), &v[2]),
                ("abs", true) => v[0].clone(),
                ("abs", false) => call("abs64"),
                ("clz", _) => call("clz64"),
                ("ctz", _) => call("ctz64"),
                ("popcount", _) => call("popcount64"),
                _ => {
                    let f = |n: &'static str, k: usize| {
                        self.need(n);
                        format!("{n}({})", v[..k].join(", "))
                    };
                    let u = unsigned;
                    match b {
                        "mul_hi" => f(if u { "umulhi64" } else { "smulhi64" }, 2),
                        "mad_hi" => {
                            self.need("add64");
                            format!("add64({}, {})", f(if u { "umulhi64" } else { "smulhi64" }, 2), v[2])
                        }
                        "add_sat" => f(if u { "uaddsat64" } else { "saddsat64" }, 2),
                        "sub_sat" => f(if u { "usubsat64" } else { "ssubsat64" }, 2),
                        "mad_sat" => f(if u { "umadsat64" } else { "smadsat64" }, 3),
                        "abs_diff" => {
                            self.need("sub64");
                            let (a, c) = (&v[0], &v[1]);
                            format!("select(sub64({a}, {c}), sub64({c}, {a}), {lt}({a}, {c}))")
                        }
                        // (a >> 1) + (b >> 1) + the carry of their low bits.
                        "hadd" | "rhadd" => {
                            let sh = if u { "lshr64" } else { "ashr64" };
                            self.need(sh);
                            self.need("add64");
                            let (a, c) = (&v[0], &v[1]);
                            let low = if b == "hadd" { format!("({a}.x & {c}.x & 1u)") } else { format!("(({a}.x | {c}.x) & 1u)") };
                            format!("add64(add64({sh}({a}, 1u), {sh}({c}, 1u)), vec2<u32>({low}, 0u))")
                        }
                        "rotate" => {
                            self.need("shl64");
                            self.need("lshr64");
                            let (a, s) = (&v[0], format!("({}.x & 63u)", v[1]));
                            // (Shifts take their counts mod 64: a rotation by 0 is a | a.)
                            format!("(shl64({a}, {s}) | lshr64({a}, 64u - {s}))")
                        }
                        b => bail!("the builtin {b} on 64-bit integers"),
                    }
                }
            });
        }
        if w > 32 {
            bail!("integers of {w} bits");
        }
        let lanes = t.lanes();
        let s = |x: &str| sext_signed(x, w, lanes);
        let back = |e: String| self.wrap_signed(t, e);
        let m = |n: u64| self.splat(t, &format!("{n}u"));
        let ones = |lanes: u32| if lanes > 1 { format!("vec{lanes}<i32>(1i)") } else { "1i".into() };
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
            // (Unsigned, so a - b or b - a wrapping: exact, where abs of the
            // signed difference overflows.)
            ("abs_diff", false) => format!("(select({a} - {b}, {b} - {a}, {} < {}) & {})", s(&v[0]), s(&v[1]), m(mask(w) as u64), a = v[0], b = v[1]),
            ("add_sat" | "sub_sat", u) => self.saturating(t, b == "add_sat", u, &v[0], &v[1]),
            ("hadd", false) => back(format!("(({a} >> {u1}) + ({b} >> {u1}) + ({a} & {b} & {i1}))", a = s(&v[0]), b = s(&v[1]), u1 = m(1), i1 = ones(lanes))),
            ("rhadd", false) => back(format!("(({a} >> {u1}) + ({b} >> {u1}) + (({a} | {b}) & {i1}))", a = s(&v[0]), b = s(&v[1]), u1 = m(1), i1 = ones(lanes))),
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
            ("mul_hi", u) if w == 32 => {
                // The high word of a 32x32 product, from 16-bit halves (lane
                // by lane); signed, less b where a < 0 and a where b < 0.
                self.needs.borrow_mut().insert("mul_hi_u32");
                let hi = if lanes > 1 {
                    let l: Vec<String> = (0..lanes).map(|k| format!("mul_hi_u32(({})[{k}], ({})[{k}])", v[0], v[1])).collect();
                    format!("vec{lanes}<u32>({})", l.join(", "))
                } else {
                    format!("mul_hi_u32({}, {})", v[0], v[1])
                };
                if u {
                    hi
                } else {
                    let (a, b, z) = (&v[0], &v[1], m(0));
                    let neg = |x: &str| format!("(bitcast<{}>({x}) < {})", if lanes > 1 { format!("vec{lanes}<i32>") } else { "i32".into() }, if lanes > 1 { format!("vec{lanes}<i32>(0)") } else { "0i".into() });
                    format!("({hi} - select({z}, {b}, {}) - select({z}, {a}, {}))", neg(a), neg(b))
                }
            }
            ("mul_hi", true) => format!("(({} * {}) >> {})", v[0], v[1], m(w as u64)),
            ("mul_hi", false) => back(format!("(({} * {}) >> {})", s(&v[0]), s(&v[1]), m(w as u64))),
            ("mad_hi", u) => {
                let hi = self.integer("mul_hi", t, &args[..2], &v[..2], u)?;
                format!("(({hi} + {}) & {})", v[2], m(mask(w) as u64))
            }
            ("mad_sat", u) if w == 32 => {
                // (A function a lane at a time.)
                let n = if u { "umadsat32" } else { "smadsat32" };
                self.need(n);
                if lanes > 1 {
                    let l: Vec<String> = (0..lanes).map(|k| format!("{n}(({})[{k}], ({})[{k}], ({})[{k}])", v[0], v[1], v[2])).collect();
                    format!("vec{lanes}<u32>({})", l.join(", "))
                } else {
                    format!("{n}({}, {}, {})", v[0], v[1], v[2])
                }
            }
            // (Narrower: exact in 32 bits.)
            ("mad_sat", true) => format!("min({} * {} + {}, {})", v[0], v[1], v[2], m(mask(w) as u64)),
            ("mad_sat", false) => {
                let it = if lanes > 1 { format!("vec{lanes}<i32>") } else { "i32".into() };
                let (lo, hi) = (-(1i64 << (w - 1)), (1i64 << (w - 1)) - 1);
                back(format!("clamp({} * {} + {}, {it}({lo}), {it}({hi}))", s(&v[0]), s(&v[1]), s(&v[2])))
            }
            // upsample(hi, lo): hi's bits above lo's, twice the width.
            ("upsample", _) if w == 32 => format!("vec2<u32>({}, {})", v[1], v[0]),
            ("upsample", _) => format!("(({} << {}) | {})", v[0], m(w as u64), v[1]),
            (b, u) => bail!("the builtin {b} ({})", if u { "unsigned" } else { "signed" }),
        })
    }


    /// convert_* to or from a 64-bit integer (one lane).
    fn convert64(&self, b: &str, t: &Type, a: &Operand, v: &str, unsigned: bool) -> Res<String> {
        let sat = b.contains("_sat");
        let to_unsigned = b.trim_start_matches("convert_").starts_with('u');
        let (from_w, to_w) = (bits(&a.ty), bits(t));
        let call = |n: &'static str, x: &str| {
            self.need(n);
            format!("{n}({x})")
        };
        let rounded = if b.contains("_rte") {
            format!("round({v})")
        } else if b.contains("_rtp") {
            format!("ceil({v})")
        } else if b.contains("_rtn") {
            format!("floor({v})")
        } else {
            v.to_string()
        };
        let lt = |s: bool| {
            let n = if s { "slt64" } else { "ult64" };
            self.need(n);
            n
        };
        let wide = |c: i64| format!("vec2<u32>({}u, {}u)", c as u32, ((c as u64) >> 32) as u32);
        Ok(match (a.ty.scalar().is_float(), t.scalar().is_float()) {
            (true, false) => call(if to_unsigned { "f32tou64" } else { "f32tos64" }, &rounded),
            (false, true) => call(if unsigned { "u64tof32" } else { "s64tof32" }, v),
            _ if from_w == 64 && to_w == 64 => {
                if sat && unsigned && !to_unsigned {
                    format!("select({v}, vec2<u32>(0xffffffffu, 0x7fffffffu), ({v}.y >> 31u) != 0u)")
                } else if sat && !unsigned && to_unsigned {
                    format!("select({v}, vec2<u32>(0u, 0u), bitcast<i32>({v}.y) < 0i)")
                } else {
                    v.to_string()
                }
            }
            _ if to_w == 64 => {
                // Widening: sign-extended from a signed source.
                if unsigned || from_w == 1 {
                    format!("vec2<u32>({v}, 0u)")
                } else {
                    let s = sext_signed(v, from_w, 1);
                    let s = if sat && to_unsigned { format!("max({s}, 0i)") } else { s };
                    format!("vec2<u32>(bitcast<u32>({s}), select(0u, 0xffffffffu, {s} < 0i))")
                }
            }
            _ => {
                // Narrowing: the low bits, or clamped.
                let m = mask(to_w);
                let low = format!("({v}.x & {m}u)");
                if !sat {
                    low
                } else {
                    let (lo, hi): (i64, i64) = if to_unsigned { (0, m as i64) } else { (-(1i64 << (to_w - 1)), (1i64 << (to_w - 1)) - 1) };
                    let l = lt(!unsigned);
                    let hi_clamped = format!("select({low}, {}u, {l}({}, {v}))", hi as u64 as u32 & m, wide(hi));
                    if unsigned { hi_clamped } else { format!("select({hi_clamped}, {}u, {l}({v}, {}))", lo as u32 & m, wide(lo)) }
                }
            }
        })
    }
    /// convert_T[_sat][_rtX]: `unsigned` if the source is (its mangled parameter).
    fn convert(&self, b: &str, t: &Type, a: &Operand, v: &str, unsigned: bool) -> Res<String> {
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
        if (!from_float && from_w == 64) || (!to_float && to_w == 64) {
            return self.convert64(b, t, a, v, unsigned);
        }
        if (!from_float && from_w > 32) || (!to_float && to_w > 32) {
            bail!("integers of {} bits", from_w.max(to_w));
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
                // (Signed or not as the parameter is mangled.)
                if unsigned { format!("{}({v})", vt("f32")) } else { format!("{}({})", vt("f32"), sext_signed(v, from_w, lanes)) }
            }
            (true, false) => {
                let (lo, hi): (i64, i64) = if to_unsigned { (0, mask(to_w) as i64) } else { (-(1i64 << (to_w - 1)), (1i64 << (to_w - 1)) - 1) };
                let x = if sat || to_w < 32 { format!("clamp({rounded}, {}({lo}.0), {}({hi}.0))", vt("f32"), vt("f32")) } else { rounded };
                let e = if to_unsigned { format!("{}({x})", vt("u32")) } else { format!("{}({}({x}))", vt("u32"), vt("i32")) };
                if to_w < 32 { format!("({e} & {})", self.splat(t, &format!("{}u", mask(to_w)))) } else { e }
            }
            (false, false) if sat => {
                // Clamped to the target's range, the source read as signed or not.
                let hi = if to_unsigned { mask(to_w) } else { mask(to_w - 1) };
                let (i, u) = (vt("i32"), vt("u32"));
                let e = if unsigned {
                    if to_w < 32 || !to_unsigned { format!("min({v}, {})", self.splat(t, &format!("{hi}u"))) } else { v.to_string() }
                } else {
                    let s = sext_signed(v, from_w, lanes);
                    let s = if to_unsigned { format!("max({s}, {i}(0))") } else { s };
                    let s = if to_w < 32 || !to_unsigned {
                        let lo = if to_unsigned { 0 } else { -(1i64 << (to_w - 1)) };
                        format!("clamp({s}, {i}({lo}), {i}({}))", hi as i64)
                    } else {
                        s
                    };
                    format!("{u}({s})")
                };
                if to_w < 32 { format!("({e} & {})", self.splat(t, &format!("{}u", mask(to_w)))) } else { e }
            }
            (false, false) => {
                if to_w < 32 {
                    format!("({v} & {})", self.splat(t, &format!("{}u", mask(to_w))))
                } else if from_w < 32 {
                    // Widening: sign-extend unless the source is unsigned.
                    if unsigned { v.to_string() } else { format!("{}({})", vt("u32"), sext_signed(v, from_w, lanes)) }
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

/// Functions generated code may call: name, the functions it calls, code.
/// 64-bit integers are `vec2<u32>` (low word, high word).
const LIBRARY: &[(&str, &[&str], &str)] = &[
    // (Quirks::flushes_rounded_up_products.) A product flushed to zero
    // that, rounded to the denormals' precision, is the smallest normal
    // number: that number. Scaled by 2^60 to see (the threshold is
    // 2^-126 - 2^-150), with fma's exact remainder for products that round
    // onto it from below. (Zero by its bits: a driver may take NaN != 0 to
    // be false.)
    (
        "fmul_ftz",
        &[],
        "fn fmul_ftz(a: f32, b: f32) -> f32 {
    let p = a * b;
    if ((bitcast<u32>(p) & 0x7fffffffu) != 0u || a == 0.0 || b == 0.0) { return p; }
    let s = min(abs(a), abs(b)) * 0x1p60f;
    let l = max(abs(a), abs(b));
    let r = s * l;
    let t = 0x1.fffffep-67f;
    if (r > t || (r == t && fma(s, l, -r) >= 0.0)) {
        return bitcast<f32>(((bitcast<u32>(a) ^ bitcast<u32>(b)) & 0x80000000u) | 0x00800000u);
    }
    return p;
}",
    ),
    (
        "mul_hi_u32",
        &[],
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
    ),
    ("add64", &[], "fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x + b.x;
    return vec2<u32>(lo, a.y + b.y + select(0u, 1u, lo < a.x));
}"),
    ("sub64", &[], "fn sub64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x - b.x, a.y - b.y - select(0u, 1u, a.x < b.x));
}"),
    ("neg64", &["sub64"], "fn neg64(a: vec2<u32>) -> vec2<u32> { return sub64(vec2<u32>(0u, 0u), a); }"),
    ("mul64", &["mul_hi_u32"], "fn mul64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x * b.x, mul_hi_u32(a.x, b.x) + a.x * b.y + a.y * b.x);
}"),
    ("shl64", &[], "fn shl64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(0u, a.x << (n - 32u)); }
    return vec2<u32>(a.x << n, (a.y << n) | (a.x >> (32u - n)));
}"),
    ("lshr64", &[], "fn lshr64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(a.y >> (n - 32u), 0u); }
    return vec2<u32>((a.x >> n) | (a.y << (32u - n)), a.y >> n);
}"),
    ("ashr64", &[], "fn ashr64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    let h = bitcast<i32>(a.y);
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(bitcast<u32>(h >> (n - 32u)), bitcast<u32>(h >> 31u)); }
    return vec2<u32>((a.x >> n) | (a.y << (32u - n)), bitcast<u32>(h >> n));
}"),
    ("ult64", &[], "fn ult64(a: vec2<u32>, b: vec2<u32>) -> bool { return a.y < b.y || (a.y == b.y && a.x < b.x); }"),
    ("slt64", &[], "fn slt64(a: vec2<u32>, b: vec2<u32>) -> bool {
    let ah = bitcast<i32>(a.y);
    let bh = bitcast<i32>(b.y);
    return ah < bh || (ah == bh && a.x < b.x);
}"),
    ("abs64", &["neg64"], "fn abs64(a: vec2<u32>) -> vec2<u32> { return select(a, neg64(a), bitcast<i32>(a.y) < 0i); }"),
    ("udivrem64", &["ult64", "sub64", "shl64"], "fn udivrem64(a: vec2<u32>, b: vec2<u32>) -> array<vec2<u32>, 2> {
    // (Both 32-bit: natively.)
    if (a.y == 0u && b.y == 0u && b.x != 0u) { return array<vec2<u32>, 2>(vec2<u32>(a.x / b.x, 0u), vec2<u32>(a.x % b.x, 0u)); }
    if (ult64(a, b)) { return array<vec2<u32>, 2>(vec2<u32>(0u, 0u), a); }
    var q = vec2<u32>(0u, 0u);
    var r = vec2<u32>(0u, 0u);
    // Bit by bit from a's highest (none of the quotient's are above it).
    let top = 63i - i32(select(32u + countLeadingZeros(a.x), countLeadingZeros(a.y), a.y != 0u));
    for (var i = top; i >= 0i; i--) {
        r = shl64(r, 1u);
        let word = select(a.x, a.y, i >= 32i);
        r.x = r.x | ((word >> (u32(i) & 31u)) & 1u);
        if (!ult64(r, b)) {
            r = sub64(r, b);
            if (i >= 32i) { q.y = q.y | (1u << (u32(i) - 32u)); } else { q.x = q.x | (1u << u32(i)); }
        }
    }
    return array<vec2<u32>, 2>(q, r);
}"),
    // The high 64 bits of a 128-bit product, from 32-bit partial products
    // (bits 32-95: p00's high word, p01 and p10, with a carry out).
    ("umulhi64", &["mul_hi_u32", "add64", "ult64"], "fn umulhi64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let p01 = vec2<u32>(a.x * b.y, mul_hi_u32(a.x, b.y));
    let p10 = vec2<u32>(a.y * b.x, mul_hi_u32(a.y, b.x));
    let p11 = vec2<u32>(a.y * b.y, mul_hi_u32(a.y, b.y));
    let m1 = add64(p01, vec2<u32>(mul_hi_u32(a.x, b.x), 0u));
    let m2 = add64(m1, p10);
    let carry = select(0u, 1u, ult64(m2, m1));
    return add64(p11, vec2<u32>(m2.y, carry));
}"),
    // Signed: less b where a < 0 and a where b < 0.
    ("smulhi64", &["umulhi64", "sub64"], "fn smulhi64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let z = vec2<u32>(0u, 0u);
    return sub64(sub64(umulhi64(a, b), select(z, b, (a.y >> 31u) != 0u)), select(z, a, (b.y >> 31u) != 0u));
}"),
    ("uaddsat64", &["add64", "ult64"], "fn uaddsat64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let r = add64(a, b);
    return select(r, vec2<u32>(0xffffffffu, 0xffffffffu), ult64(r, a));
}"),
    // (Signed overflow: the result's sign differs from both operands', or
    // for a subtraction, from a's where b's differs from a's.)
    ("saddsat64", &["add64"], "fn saddsat64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let r = add64(a, b);
    let sat = select(vec2<u32>(0xffffffffu, 0x7fffffffu), vec2<u32>(0u, 0x80000000u), (a.y >> 31u) != 0u);
    return select(r, sat, (((a.y ^ r.y) & (b.y ^ r.y)) >> 31u) != 0u);
}"),
    ("usubsat64", &["sub64", "ult64"], "fn usubsat64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return select(sub64(a, b), vec2<u32>(0u, 0u), ult64(a, b));
}"),
    ("ssubsat64", &["sub64"], "fn ssubsat64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let r = sub64(a, b);
    let sat = select(vec2<u32>(0xffffffffu, 0x7fffffffu), vec2<u32>(0u, 0x80000000u), (a.y >> 31u) != 0u);
    return select(r, sat, (((a.y ^ b.y) & (a.y ^ r.y)) >> 31u) != 0u);
}"),
    // a * b + c saturated: the 128-bit sum fits when its high half is the
    // low half's sign (or zero, unsigned).
    ("umadsat64", &["umulhi64", "mul64", "add64", "ult64"], "fn umadsat64(a: vec2<u32>, b: vec2<u32>, c: vec2<u32>) -> vec2<u32> {
    let hi = umulhi64(a, b);
    let lo = mul64(a, b);
    let r = add64(lo, c);
    return select(r, vec2<u32>(0xffffffffu, 0xffffffffu), (hi.x | hi.y) != 0u || ult64(r, lo));
}"),
    ("smadsat64", &["smulhi64", "mul64", "add64", "ult64"], "fn smadsat64(a: vec2<u32>, b: vec2<u32>, c: vec2<u32>) -> vec2<u32> {
    let lo = mul64(a, b);
    let r = add64(lo, c);
    let ones = vec2<u32>(0xffffffffu, 0xffffffffu);
    let z = vec2<u32>(0u, 0u);
    let hi = add64(add64(smulhi64(a, b), select(z, ones, (c.y >> 31u) != 0u)), vec2<u32>(select(0u, 1u, ult64(r, lo)), 0u));
    let fits = all(hi == select(z, ones, (r.y >> 31u) != 0u));
    let sat = select(vec2<u32>(0xffffffffu, 0x7fffffffu), vec2<u32>(0u, 0x80000000u), (hi.y >> 31u) != 0u);
    return select(sat, r, fits);
}"),
    ("umadsat32", &["mul_hi_u32"], "fn umadsat32(a: u32, b: u32, c: u32) -> u32 {
    let lo = a * b;
    let r = lo + c;
    return select(r, 0xffffffffu, mul_hi_u32(a, b) != 0u || r < lo);
}"),
    // (In 64 bits, where a * b + c is exact.)
    ("smadsat32", &["mul64", "add64", "slt64"], "fn smadsat32(a: u32, b: u32, c: u32) -> u32 {
    let x = vec2<u32>(a, select(0u, 0xffffffffu, (a >> 31u) != 0u));
    let y = vec2<u32>(b, select(0u, 0xffffffffu, (b >> 31u) != 0u));
    let z = vec2<u32>(c, select(0u, 0xffffffffu, (c >> 31u) != 0u));
    let r = add64(mul64(x, y), z);
    if (slt64(r, vec2<u32>(0x80000000u, 0xffffffffu))) { return 0x80000000u; }
    if (slt64(vec2<u32>(0x7fffffffu, 0u), r)) { return 0x7fffffffu; }
    return r.x;
}"),
    ("udiv64", &["udivrem64"], "fn udiv64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return udivrem64(a, b)[0]; }"),
    ("urem64", &["udivrem64"], "fn urem64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return udivrem64(a, b)[1]; }"),
    ("sdiv64", &["udivrem64", "abs64", "neg64"], "fn sdiv64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let q = udivrem64(abs64(a), abs64(b))[0];
    return select(q, neg64(q), (bitcast<i32>(a.y) < 0i) != (bitcast<i32>(b.y) < 0i));
}"),
    ("srem64", &["udivrem64", "abs64", "neg64"], "fn srem64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let r = udivrem64(abs64(a), abs64(b))[1];
    return select(r, neg64(r), bitcast<i32>(a.y) < 0i);
}"),
    ("u64tof32", &[], "fn u64tof32(a: vec2<u32>) -> f32 { return f32(a.y) * 4294967296.0 + f32(a.x); }"),
    ("s64tof32", &["u64tof32", "neg64"], "fn s64tof32(a: vec2<u32>) -> f32 {
    if (bitcast<i32>(a.y) < 0i) { return -u64tof32(neg64(a)); }
    return u64tof32(a);
}"),
    ("f32tou64", &[], "fn f32tou64(f: f32) -> vec2<u32> {
    let x = max(f, 0.0);
    if (x >= 18446744073709551616.0) { return vec2<u32>(0xffffffffu, 0xffffffffu); }
    let hi = floor(x / 4294967296.0);
    return vec2<u32>(u32(x - hi * 4294967296.0), u32(hi));
}"),
    ("f32tos64", &["f32tou64", "neg64"], "fn f32tos64(f: f32) -> vec2<u32> {
    if (f >= 9223372036854775808.0) { return vec2<u32>(0xffffffffu, 0x7fffffffu); }
    if (f <= -9223372036854775808.0) { return vec2<u32>(0u, 0x80000000u); }
    if (f < 0.0) { return neg64(f32tou64(-f)); }
    return f32tou64(f);
}"),
    ("clz64", &[], "fn clz64(a: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(select(32u + countLeadingZeros(a.x), countLeadingZeros(a.y), a.y != 0u), 0u);
}"),
    ("ctz64", &[], "fn ctz64(a: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(select(32u + min(countTrailingZeros(a.y), 32u), countTrailingZeros(a.x), a.x != 0u), 0u);
}"),
    ("popcount64", &[], "fn popcount64(a: vec2<u32>) -> vec2<u32> { return vec2<u32>(countOneBits(a.x) + countOneBits(a.y), 0u); }"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencl::{build, test_clang};

    /// A kernel's WGSL, checked by naga as wgpu would (with the quirks' code too).
    pub(crate) fn wgsl(src: &str, kernel: &str, launch: &Launch) -> String {
        let m = build(&test_clang(), src, "", None).unwrap();
        let w = compile(&m, kernel, launch, Quirks { flushes_rounded_up_products: true }).unwrap_or_else(|e| panic!("{kernel}: {e}"));
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
__kernel void builtins(__global float4 *f, __global int4 *i, __global long *l) {
    int k = get_global_id(0);
    prefetch(f, 4);
    float4 x = f[k];
    f[k] = normalize(x) + (float4)(length(x), fast_length(x.xy), distance(x, f[k + 1]), nextafter(x.x, 0.0f))
        + shuffle(x, (uint4)(3, 2, 1, 0)) + shuffle2(x, f[k + 2], (uint4)(7, 0, 5, 2)) + nextafter(x, (float4)(1.0f));
    i[k] = hadd(i[k], i[k + 1]) + rhadd(i[k], (int4)(3)) + isnan(x) + isless(x, (float4)(0.0f)) + (x != x);
    l[k] = l[k] * 3 + (long)i[k].x;
}
__kernel void longargs(long l, ulong ul, long2 l2, ulong16 ul16, __global float *result, __global float2 *r2, __global float16 *r16) {
    result[0] = convert_float(l);
    result[1] = convert_float(ul);
    r2[0] = convert_float2(l2);
    r16[0] = convert_float16(ul16);
}
__kernel void scalars(__global float4 *f, __global int4 *i) {
    int k = get_global_id(0);
    f[k + 1] = cross(f[k], f[k + 2]) + (float4)(dot(f[k].x, f[k].y), dot(f[k], f[k]), length(f[k].x), 0.0f);
    f[k] = fmax(f[k], 1.0f) + mix(f[k], f[k + 1], 0.25f) + step(0.5f, f[k]) + smoothstep(0.0f, 1.0f, f[k]) + clamp(f[k], 0.0f, 2.0f);
    i[k] = clamp(i[k], 0, 9) + min(i[k], 3) + max(i[k], -3);
}
__kernel void selects(__global uchar4 *a, __global char4 *m, __global float4 *f, __global int4 *i, __global long2 *l, __global short *s) {
    int k = get_global_id(0);
    i[k + 1] = isnotequal(f[k], f[k + 1]) + islessgreater(f[k], f[k + 2]) + bitselect(i[k], i[k + 1], i[k + 2]) + (int4)(any(i[k]), all(m[k]), any(l[k]), isunordered(f[k].x, 1.0f));
    f[k + 2] = bitselect(f[k], f[k + 1], f[k + 2]);
    a[k] = select(a[k], a[k + 1], m[k]);
    f[k] = select(f[k], f[k + 1], i[k]);
    l[k] = select(l[k], l[k + 1], l[k + 2]);
    s[k] = select(s[k], s[k + 1], s[k + 2]);
}
__kernel void reduces(__global int4 *a, __global int *out) {
    int k = get_global_id(0);
    int4 v = a[k];
    out[k] = (v.x | v.y | v.z | v.w) + (v.x + v.y + v.z + v.w) + max(max(v.x, v.y), max(v.z, v.w));
}
__kernel void addresses(__global ulong *out) {
    __local int l[4];
    __local long m[2];
    out[0] = (ulong)&l[1] % 4;
    out[1] = ((ulong)m) & 7;
    l[get_local_id(0) & 3] = 1;
    m[0] = 2;
}
__kernel void longlogic(__global long4 *a, __global long4 *b, __global long *s, __global long2 *c) {
    int k = get_global_id(0);
    a[k] = (a[k] && b[k]) + (a[k] || b[k]) + !a[k] + (a[k] < b[k]) + (a[k] == b[k]);
    s[k] = (s[k] && s[k + 1]) + !s[k];
    c[k] = c[k] > c[k + 1] ? c[k] : c[k + 1];
}
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
        for k in ["add", "reduce", "bytes", "misc", "restricted", "builtins", "longargs", "scalars", "selects", "reduces", "addresses", "longlogic"] {
            wgsl(KERNELS, k, &launch);
        }
    }

    #[test]
    fn arguments_are_laid_out_as_opencl_does() {
        let m = build(&test_clang(), KERNELS, "", None).unwrap();
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
    fn kernel_attributes_read_as_written() {
        let src = "__kernel __attribute__((vec_type_hint(uint4))) __attribute__((reqd_work_group_size(4, 2, 1))) void k(__global int *p) { p[0] = 1; }\n\
                   __kernel __attribute__((vec_type_hint(float))) __attribute__((work_group_size_hint(8, 1, 1))) void j(__global int *p) { p[0] = 2; }";
        let m = build(&test_clang(), src, "", None).unwrap();
        assert_eq!(kernel_info(&m, "k").unwrap().attributes, "reqd_work_group_size(4,2,1) vec_type_hint(uint4)");
        assert_eq!(kernel_info(&m, "j").unwrap().attributes, "work_group_size_hint(8,1,1) vec_type_hint(float)");
    }

    #[test]
    fn builtins_demangle() {
        assert_eq!(demangle("_Z13get_global_idj"), Some(("get_global_id".into(), vec!['j'])));
        assert_eq!(demangle("_Z3minDv4_iS_"), Some(("min".into(), vec!['i'])));
        assert_eq!(demangle("_Z10atomic_addPU3AS1Vii"), Some(("atomic_add".into(), vec!['i', 'i'])));
    }
}

/// Builtins written for one lane: vector calls go a lane at a time.
const SCALAR_ONLY: &[&str] = &["nextafter"];

/// The size of the elements an async copy's first parameter points to,
/// from its mangled name (`PU3AS3Dv4_f`: float4, 16 bytes; three-element
/// vectors take four elements' room).
fn elem_size(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("_Z")?;
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    let len: usize = rest[..digits].parse().ok()?;
    let mut p = rest[digits + len..].chars().peekable();
    let scalar = |c: char| match c {
        'c' | 'h' => Some(1),
        's' | 't' | 'D' => Some(2),
        'i' | 'j' | 'f' => Some(4),
        'l' | 'm' | 'd' => Some(8),
        _ => None,
    };
    while let Some(c) = p.next() {
        match c {
            'P' | 'K' | 'V' => {}
            'U' => {
                let n: String = std::iter::from_fn(|| p.next_if(|c| c.is_ascii_digit())).collect();
                for _ in 0..n.parse::<usize>().ok()? {
                    p.next();
                }
            }
            'D' if p.peek() == Some(&'v') => {
                p.next();
                let n: String = std::iter::from_fn(|| p.next_if(|c| c.is_ascii_digit())).collect();
                let n: u32 = n.parse().ok()?;
                p.next(); // '_'
                let e = if p.peek() == Some(&'D') {
                    p.next();
                    p.next();
                    2
                } else {
                    scalar(p.next()?)?
                };
                return Some(e * if n == 3 { 4 } else { n });
            }
            'D' => return Some(2),
            c => return scalar(c),
        }
    }
    None
}

/// Values a `let` can hold: defined once by a single assignment (not a
/// phi, an insertelement, a lane-at-a-time wide vector, a cmpxchg's
/// pair), and used only in their own block (WGSL scopes nest as blocks'
/// code does, but a use in another block may be in a scope the `let`
/// doesn't reach).
fn local_values(f: &Function) -> BTreeSet<usize> {
    let mut block_of = HashMap::new();
    let mut candidates = BTreeSet::new();
    for (b, block) in f.blocks.iter().enumerate() {
        for i in &block.insts {
            let Some(r) = i.result else { continue };
            block_of.insert(r, b);
            let constant_lane = matches!(&i.op, Op::InsertElement(_, _, k) if matches!(k.v, Value::Const(Const::Int(_))));
            let single = !matches!(i.op, Op::Phi(_) | Op::CmpXchg(..)) && (constant_lane || !matches!(i.op, Op::InsertElement(..))) && Gen::wide(i).is_none();
            if single {
                candidates.insert(r);
            }
        }
    }
    let mut uses = |o: &Operand, b: usize, phi: bool| {
        if let Value::Local(r) = o.v
            && (phi || block_of.get(&r) != Some(&b))
        {
            candidates.remove(&r);
        }
    };
    for (b, block) in f.blocks.iter().enumerate() {
        for i in &block.insts {
            let phi = matches!(i.op, Op::Phi(_));
            for o in operands(&i.op) {
                uses(o, b, phi);
            }
        }
        match &block.term {
            Term::CondBr(c, ..) | Term::Switch(c, ..) | Term::Ret(Some(c)) => uses(c, b, false),
            _ => {}
        }
    }
    candidates
}

fn operands(op: &Op) -> Vec<&Operand> {
    match op {
        Op::Bin(_, a, b) | Op::ICmp(_, a, b) | Op::FCmp(_, a, b) | Op::Store(a, b, _) | Op::ExtractElement(a, b) | Op::AtomicRmw(_, a, b) => vec![a, b],
        Op::FNeg(a) | Op::Cast(_, a) | Op::Load(a, _) | Op::Freeze(a) | Op::ExtractValue(a, _) => vec![a],
        Op::Select(a, b, c) | Op::InsertElement(a, b, c) | Op::CmpXchg(a, b, c) => vec![a, b, c],
        Op::Shuffle(a, b, _) | Op::InsertValue(a, b, _) => vec![a, b],
        Op::Gep(_, a, idx) => std::iter::once(a).chain(idx).collect(),
        Op::Phi(inc) => inc.iter().map(|(o, _)| o).collect(),
        Op::Call(_, args) => args.iter().collect(),
        Op::Alloca(..) | Op::Fence => vec![],
    }
}

/// Builtins whose vector forms take scalars for some arguments.
const SPLATTED: &[&str] = &["min", "max", "clamp", "mix", "step", "smoothstep", "fmin", "fmax", "fdim", "fmod", "pow", "powr", "atan2", "hypot", "copysign"];

/// A kernel's attributes from its metadata, as OpenCL C spells them.
fn attributes(m: &Module, f: &Function, required: Option<[u32; 3]>) -> String {
    let mut out = Vec::new();
    if let Some([x, y, z]) = required {
        out.push(format!("reqd_work_group_size({x},{y},{z})"));
    }
    if let Some(&n) = f.metadata.get("work_group_size_hint")
        && let [x, y, z] = m.meta_ints(n)[..]
    {
        out.push(format!("work_group_size_hint({x},{y},{z})"));
    }
    // vec_type_hint: !{<4 x i32> undef, i32 1}: the type, and 1 if signed.
    if let Some(text) = f.metadata.get("vec_type_hint").and_then(|n| m.metadata_text.get(n)) {
        let (ty, signed) = text.rsplit_once(',').map_or((text.as_str(), "1"), |(t, s)| (t, s.trim()));
        let ty = ty.trim().trim_end_matches("undef").trim_end_matches("poison").trim();
        let (lanes, scalar) = match ty.strip_prefix('<').and_then(|t| t.strip_suffix('>')).and_then(|t| t.split_once(" x ")) {
            Some((n, e)) => (n.trim().to_string(), e.trim()),
            None => (String::new(), ty),
        };
        let unsigned = signed.ends_with('0');
        let name = match scalar {
            "i8" => "char",
            "i16" => "short",
            "i32" => "int",
            "i64" => "long",
            "half" => "half",
            "float" => "float",
            "double" => "double",
            _ => "",
        };
        if !name.is_empty() {
            let u = if unsigned && scalar.starts_with('i') { "u" } else { "" };
            out.push(format!("vec_type_hint({u}{name}{lanes})"));
        }
    }
    out.join(" ")
}

/// Whether a float (or each lane) is a NaN, by its bits: a driver may take
/// `x != x` to be false.
fn is_nan(v: &str, lanes: u32) -> String {
    let u = if lanes > 1 { format!("vec{lanes}<u32>") } else { "u32".into() };
    format!("((bitcast<{u}>({v}) & {u}(0x7fffffffu)) > {u}(0x7f800000u))")
}

/// fmin or fmax (`f`: "min" or "max") the way OpenCL and llvm.minnum have
/// it: a NaN operand gives the other one (WGSL leaves NaNs unspecified).
fn min_max_num(f: &str, a: &str, b: &str, lanes: u32) -> String {
    format!("select(select({f}({a}, {b}), {b}, {}), {a}, {})", is_nan(a, lanes), is_nan(b, lanes))
}
