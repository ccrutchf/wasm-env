//! LLVM IR, as text, for what clang emits for OpenCL C kernels (`-target
//! spir -O2`): types and their layout, constants, global variables, and
//! functions of basic blocks. Enough of the language for kernels; anything
//! else is reported, so a kernel that uses it fails to build with a reason.

use std::collections::HashMap;

pub type Res<T> = Result<T, String>;

macro_rules! bail {
    ($($arg:tt)*) => { return Err(format!($($arg)*)) };
}
#[allow(unused_imports)]
pub(crate) use bail;

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Void,
    Int(u32),
    Half,
    Float,
    Double,
    /// A pointer, in an address space (OpenCL's: 0 private, 1 global,
    /// 2 constant, 3 local).
    Ptr(u32),
    Vector(u32, Box<Type>),
    Array(u64, Box<Type>),
    Struct(Vec<Type>, bool),
    Named(String),
    Label,
    Metadata,
}

impl Type {
    pub fn is_float(&self) -> bool {
        matches!(self, Type::Half | Type::Float | Type::Double)
    }

    /// A vector's element type, or the type itself.
    pub fn scalar(&self) -> &Type {
        match self {
            Type::Vector(_, t) => t,
            t => t,
        }
    }

    pub fn lanes(&self) -> u32 {
        match self {
            Type::Vector(n, _) => *n,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Int(u64),
    /// A float constant's value (as f64, which holds every float and half).
    Float(f64),
    Null,
    Undef,
    Zero,
    Vector(Vec<Const>),
    Array(Vec<Const>),
    Struct(Vec<Const>),
    Bytes(Vec<u8>),
    Global(String),
    /// A constant expression: getelementptr (source type, operands), or a
    /// cast of one operand.
    Gep(Type, Vec<Operand>),
    Cast(String, Box<Operand>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// An instruction's result, by the function's value number.
    Local(usize),
    Arg(usize),
    Const(Const),
    /// A WGSL expression the code generator made (one lane of a wide
    /// vector): never in parsed IR.
    Wgsl(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Operand {
    pub ty: Type,
    pub v: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// add, sub, mul, udiv, sdiv, urem, srem, shl, lshr, ashr, and, or,
    /// xor, fadd, fsub, fmul, fdiv, frem.
    Bin(String, Operand, Operand),
    FNeg(Operand),
    ICmp(String, Operand, Operand),
    FCmp(String, Operand, Operand),
    Select(Operand, Operand, Operand),
    /// zext, sext, trunc, fptosi, fptoui, sitofp, uitofp, fptrunc, fpext,
    /// bitcast, ptrtoint, inttoptr, addrspacecast: to the instruction's type.
    Cast(String, Operand),
    Gep(Type, Operand, Vec<Operand>),
    Load(Operand, u32),
    Store(Operand, Operand, u32),
    Alloca(Type, u64, u32),
    Phi(Vec<(Operand, usize)>),
    Call(String, Vec<Operand>),
    ExtractElement(Operand, Operand),
    InsertElement(Operand, Operand, Operand),
    Shuffle(Operand, Operand, Vec<i64>),
    ExtractValue(Operand, Vec<u32>),
    InsertValue(Operand, Operand, Vec<u32>),
    /// atomicrmw: the operation, the pointer, the operand.
    AtomicRmw(String, Operand, Operand),
    /// cmpxchg: pointer, expected, new; the result is { value, success }.
    CmpXchg(Operand, Operand, Operand),
    Fence,
    Freeze(Operand),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inst {
    pub result: Option<usize>,
    pub ty: Type,
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Br(usize),
    CondBr(Operand, usize, usize),
    Switch(Operand, usize, Vec<(u64, usize)>),
    Ret(Option<Operand>),
    Unreachable,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Term,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub ret: Type,
    pub params: Vec<Type>,
    pub kernel: bool,
    pub blocks: Vec<Block>,
    /// Each value number's type.
    pub values: Vec<Type>,
    /// Metadata attachments by name (kernel_arg_type and the like).
    pub metadata: HashMap<String, usize>,
    /// Parameters passed by value as pointers to a copy (`byval(T)`): T.
    pub byval: HashMap<usize, Type>,
}

#[derive(Clone, Debug)]
pub struct Global {
    pub name: String,
    pub ty: Type,
    pub space: u32,
    pub constant: bool,
    pub init: Option<Const>,
    pub align: u32,
}

#[derive(Default, Debug)]
pub struct Module {
    pub types: HashMap<String, Type>,
    pub globals: Vec<Global>,
    pub functions: Vec<Function>,
    /// Numbered metadata nodes, as their operands' text.
    pub metadata: HashMap<usize, Vec<MetaItem>>,
    /// The same nodes as written (for operands `MetaItem` doesn't keep: types).
    pub metadata_text: HashMap<usize, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MetaItem {
    Str(String),
    Int(i64),
    Node(usize),
    Other,
}

impl Module {
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|f| f.name == name)
    }

    /// A named type's definition.
    pub fn resolve<'a>(&'a self, t: &'a Type) -> Res<&'a Type> {
        match t {
            Type::Named(n) => self.types.get(n).ok_or_else(|| format!("unknown type %{n}")),
            t => Ok(t),
        }
    }

    /// (size, alignment) in bytes, as the spir target lays types out.
    pub fn layout(&self, t: &Type) -> Res<(u64, u64)> {
        Ok(match self.resolve(t)? {
            Type::Int(1) | Type::Int(8) => (1, 1),
            Type::Int(16) | Type::Half => (2, 2),
            Type::Int(32) | Type::Float | Type::Ptr(_) => (4, 4),
            Type::Int(64) | Type::Double => (8, 8),
            Type::Int(n) => bail!("unsupported integer type i{n}"),
            Type::Vector(n, e) => {
                let (s, _) = self.layout(e)?;
                // Three-element vectors take four elements' room.
                let size = s * if *n == 3 { 4 } else { *n as u64 };
                (size, size)
            }
            Type::Array(n, e) => {
                let (s, a) = self.layout(e)?;
                (s * n, a)
            }
            Type::Struct(fields, packed) => {
                let (mut size, mut align) = (0u64, 1u64);
                for f in fields {
                    let (s, a) = self.layout(f)?;
                    let a = if *packed { 1 } else { a };
                    size = size.next_multiple_of(a) + s;
                    align = align.max(a);
                }
                (size.next_multiple_of(align), align)
            }
            t => bail!("type {t:?} has no size"),
        })
    }

    /// A struct field's byte offset.
    pub fn field_offset(&self, t: &Type, index: usize) -> Res<u64> {
        let Type::Struct(fields, packed) = self.resolve(t)? else { bail!("not a struct: {t:?}") };
        let mut offset = 0u64;
        for (i, f) in fields.iter().enumerate() {
            let (s, a) = self.layout(f)?;
            offset = offset.next_multiple_of(if *packed { 1 } else { a });
            if i == index {
                return Ok(offset);
            }
            offset += s;
        }
        bail!("no field {index} in {t:?}")
    }

    /// A metadata node's strings (kernel_arg_type and the like).
    pub fn meta_strings(&self, node: usize) -> Vec<String> {
        self.metadata.get(&node).map_or_else(Vec::new, |items| {
            items.iter().map(|i| if let MetaItem::Str(s) = i { s.clone() } else { String::new() }).collect()
        })
    }

    pub fn meta_ints(&self, node: usize) -> Vec<i64> {
        self.metadata.get(&node).map_or_else(Vec::new, |items| items.iter().map(|i| if let MetaItem::Int(n) = i { *n } else { 0 }).collect())
    }
}

// ---- Lexing ----

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// %name or %N
    Local(String),
    /// @name
    Global(String),
    /// !name or !N
    Meta(String),
    /// #N
    Attr(String),
    Word(String),
    Int(i128),
    Float(f64),
    Str(Vec<u8>),
    /// c"..."
    CStr(Vec<u8>),
    Punct(char),
    Ellipsis,
}

fn lex(s: &str) -> Res<Vec<Tok>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let name = |i: &mut usize| -> Res<String> {
        if b.get(*i) == Some(&b'"') {
            let (bytes, n) = string(&b[*i..])?;
            *i += n;
            return String::from_utf8(bytes).map_err(|e| e.to_string());
        }
        let start = *i;
        while *i < b.len() && (b[*i].is_ascii_alphanumeric() || b"._$-".contains(&b[*i])) {
            *i += 1;
        }
        Ok(s[start..*i].to_string())
    };
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b';' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'%' | b'@' | b'!' | b'#' => {
                i += 1;
                if c == b'!' && b.get(i) == Some(&b'"') {
                    let (bytes, n) = string(&b[i..])?;
                    i += n;
                    out.push(Tok::Str(bytes));
                    continue;
                }
                if c == b'!' && b.get(i) == Some(&b'{') {
                    out.push(Tok::Punct('!'));
                    continue;
                }
                let n = name(&mut i)?;
                out.push(match c {
                    b'%' => Tok::Local(n),
                    b'@' => Tok::Global(n),
                    b'!' => Tok::Meta(n),
                    _ => Tok::Attr(n),
                });
            }
            b'"' => {
                let (bytes, n) = string(&b[i..])?;
                i += n;
                out.push(Tok::Str(bytes));
            }
            b'c' if b.get(i + 1) == Some(&b'"') => {
                let (bytes, n) = string(&b[i + 1..])?;
                i += 1 + n;
                out.push(Tok::CStr(bytes));
            }
            b'.' if s[i..].starts_with("...") => {
                i += 3;
                out.push(Tok::Ellipsis);
            }
            b'0'..=b'9' | b'-' | b'+' => {
                let start = i;
                if s[i..].starts_with("0x") {
                    i += 2;
                    // 0xH (half), 0xK/0xL/0xM (wider floats): a kind letter.
                    let kind = if b.get(i).is_some_and(|c| b"HKLMR".contains(c)) {
                        i += 1;
                        Some(b[i - 1])
                    } else {
                        None
                    };
                    let hs = i;
                    while i < b.len() && b[i].is_ascii_hexdigit() {
                        i += 1;
                    }
                    let bits = u128::from_str_radix(&s[hs..i], 16).map_err(|e| e.to_string())?;
                    out.push(match kind {
                        None => Tok::Float(f64::from_bits(bits as u64)),
                        Some(b'H') => Tok::Float(half_to_f64(bits as u16)),
                        Some(k) => bail!("unsupported float constant kind 0x{}", k as char),
                    });
                    continue;
                }
                i += 1;
                while i < b.len() && (b[i].is_ascii_digit() || b".eE".contains(&b[i]) || (b"+-".contains(&b[i]) && b"eE".contains(&b[i - 1]))) {
                    i += 1;
                }
                let t = &s[start..i];
                if t.contains(['.', 'e', 'E']) {
                    out.push(Tok::Float(t.parse().map_err(|_| format!("bad number {t}"))?));
                } else {
                    out.push(Tok::Int(t.parse().map_err(|_| format!("bad number {t}"))?));
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c == b'.' => {
                let n = name(&mut i)?;
                // A label: "name:".
                out.push(Tok::Word(n));
            }
            _ => {
                out.push(Tok::Punct(c as char));
                i += 1;
            }
        }
    }
    Ok(out)
}

/// A quoted string with \XX escapes: its bytes and the length consumed.
fn string(b: &[u8]) -> Res<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < b.len() && b[i] != b'"' {
        if b[i] == b'\\' && i + 2 < b.len() && b[i + 1] != b'\\' {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).map_err(|e| e.to_string())?;
            out.push(u8::from_str_radix(h, 16).map_err(|e| e.to_string())?);
            i += 3;
        } else if b[i] == b'\\' {
            out.push(b'\\');
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    if i >= b.len() {
        bail!("unterminated string");
    }
    Ok((out, i + 1))
}

fn half_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f64;
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        31 => if frac == 0.0 { f64::INFINITY } else { f64::NAN },
        e => (1.0 + frac / 1024.0) * 2f64.powi(e - 15),
    }
}

// ---- Parsing ----

/// Attributes that may sit between a parameter's type and its value, or
/// after a function's parameters.
const PARAM_ATTRS: &[&str] = &[
    "noundef", "nonnull", "readonly", "writeonly", "readnone", "noalias", "nocapture", "signext", "zeroext", "inreg", "returned",
    "nofree", "immarg", "byval", "byref", "sret", "align", "dereferenceable", "dereferenceable_or_null", "captures", "range",
    "noext", "nofpclass", "initializes", "dead_on_unwind", "writable", "elementtype", "inalloca", "preallocated",
];

struct Parser<'a> {
    t: &'a [Tok],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Tok> {
        self.t.get(self.i)
    }

    fn peek_at(&self, n: usize) -> Option<&'a Tok> {
        self.t.get(self.i + n)
    }

    fn next(&mut self) -> Res<&'a Tok> {
        let t = self.t.get(self.i).ok_or("unexpected end of IR")?;
        self.i += 1;
        Ok(t)
    }

    fn word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(x)) if x == w)
    }

    fn eat_word(&mut self, w: &str) -> bool {
        let yes = self.word(w);
        if yes {
            self.i += 1;
        }
        yes
    }

    fn punct(&self, c: char) -> bool {
        self.peek() == Some(&Tok::Punct(c))
    }

    fn eat(&mut self, c: char) -> bool {
        let yes = self.punct(c);
        if yes {
            self.i += 1;
        }
        yes
    }

    fn expect(&mut self, c: char) -> Res<()> {
        if !self.eat(c) {
            bail!("expected '{c}', found {:?}", self.peek());
        }
        Ok(())
    }

    fn int(&mut self) -> Res<i128> {
        match self.next()? {
            Tok::Int(n) => Ok(*n),
            t => bail!("expected a number, found {t:?}"),
        }
    }

    /// Skips a balanced (...) group, if one is next.
    fn skip_group(&mut self) {
        if !self.punct('(') {
            return;
        }
        let mut depth = 0;
        while let Some(t) = self.peek() {
            self.i += 1;
            match t {
                Tok::Punct('(') => depth += 1,
                Tok::Punct(')') => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// Parameter attributes (and their arguments), if any.
    fn skip_param_attrs(&mut self) -> Res<()> {
        self.param_attrs().map(|_| ())
    }

    /// Parameter attributes (and their arguments), if any: the type of a
    /// `byval(T)` among them.
    fn param_attrs(&mut self) -> Res<Option<Type>> {
        let mut byval = None;
        loop {
            match self.peek() {
                Some(Tok::Word(w)) if PARAM_ATTRS.contains(&w.as_str()) => {
                    self.i += 1;
                    if w == "align" && matches!(self.peek(), Some(Tok::Int(_))) {
                        self.i += 1;
                    } else if w == "byval" && self.punct('(') {
                        self.i += 1;
                        byval = Some(self.ty()?);
                        self.expect(')')?;
                    } else {
                        self.skip_group();
                    }
                }
                _ => return Ok(byval),
            }
        }
    }

    fn ty(&mut self) -> Res<Type> {
        let t = match self.next()? {
            Tok::Word(w) => match w.as_str() {
                "void" => Type::Void,
                "half" | "bfloat" => Type::Half,
                "float" => Type::Float,
                "double" => Type::Double,
                "label" => Type::Label,
                // An opaque target type (OpenCL's events): a 32-bit value here.
                "target" => {
                    self.skip_group();
                    Type::Int(32)
                }
                "metadata" => Type::Metadata,
                "ptr" => {
                    if self.eat_word("addrspace") {
                        self.expect('(')?;
                        let n = self.int()? as u32;
                        self.expect(')')?;
                        Type::Ptr(n)
                    } else {
                        Type::Ptr(0)
                    }
                }
                w if w.starts_with('i') && w[1..].parse::<u32>().is_ok() => Type::Int(w[1..].parse().unwrap()),
                w => bail!("unknown type {w}"),
            },
            Tok::Local(n) => Type::Named(n.clone()),
            Tok::Punct('<') => {
                if self.eat('{') {
                    let t = self.struct_body(true)?;
                    self.expect('>')?;
                    t
                } else {
                    let n = self.int()? as u32;
                    if !self.eat_word("x") {
                        bail!("expected x in vector type");
                    }
                    let e = self.ty()?;
                    self.expect('>')?;
                    Type::Vector(n, Box::new(e))
                }
            }
            Tok::Punct('[') => {
                let n = self.int()? as u64;
                if !self.eat_word("x") {
                    bail!("expected x in array type");
                }
                let e = self.ty()?;
                self.expect(']')?;
                Type::Array(n, Box::new(e))
            }
            Tok::Punct('{') => self.struct_body(false)?,
            t => bail!("expected a type, found {t:?}"),
        };
        // A function type (in a call's signature): "T (args...)".
        if self.punct('(') {
            self.skip_group();
            return Ok(Type::Void);
        }
        Ok(t)
    }

    fn struct_body(&mut self, packed: bool) -> Res<Type> {
        let mut fields = Vec::new();
        if !self.eat('}') {
            loop {
                fields.push(self.ty()?);
                if self.eat('}') {
                    break;
                }
                self.expect(',')?;
            }
        }
        Ok(Type::Struct(fields, packed))
    }

    /// A constant of type `ty`.
    fn constant(&mut self, ty: &Type, f: &mut FnState) -> Res<Const> {
        Ok(match self.next()? {
            Tok::Int(n) => {
                if ty.is_float() {
                    Const::Float(*n as f64)
                } else {
                    Const::Int(*n as u64)
                }
            }
            Tok::Float(x) => Const::Float(*x),
            Tok::Word(w) => match w.as_str() {
                "true" => Const::Int(1),
                "false" => Const::Int(0),
                "null" => Const::Null,
                "undef" | "poison" => Const::Undef,
                "zeroinitializer" => Const::Zero,
                "splat" => {
                    self.expect('(')?;
                    let et = self.ty()?;
                    let c = self.constant(&et, f)?;
                    self.expect(')')?;
                    Const::Vector(vec![c; ty.lanes() as usize])
                }
                "getelementptr" => {
                    while self.eat_word("inbounds") || self.eat_word("nuw") || self.eat_word("nusw") {}
                    if self.eat_word("inrange") {
                        self.skip_group();
                    }
                    self.expect('(')?;
                    let src = self.ty()?;
                    let mut ops = Vec::new();
                    while self.eat(',') {
                        if self.eat_word("inrange") {
                            self.skip_group();
                        }
                        ops.push(self.operand(f)?);
                    }
                    self.expect(')')?;
                    Const::Gep(src, ops)
                }
                "bitcast" | "addrspacecast" | "ptrtoint" | "inttoptr" => {
                    let op = w.clone();
                    self.expect('(')?;
                    let v = self.operand(f)?;
                    self.eat_word("to");
                    self.ty()?;
                    self.expect(')')?;
                    Const::Cast(op, Box::new(v))
                }
                w => bail!("unsupported constant {w}"),
            },
            Tok::Global(n) => Const::Global(n.clone()),
            Tok::CStr(b) => Const::Bytes(b.clone()),
            Tok::Punct('<') => {
                let items = self.const_list('>', f)?;
                Const::Vector(items)
            }
            Tok::Punct('[') => Const::Array(self.const_list(']', f)?),
            Tok::Punct('{') => Const::Struct(self.const_list('}', f)?),
            t => bail!("expected a constant, found {t:?}"),
        })
    }

    fn const_list(&mut self, close: char, f: &mut FnState) -> Res<Vec<Const>> {
        let mut out = Vec::new();
        if self.eat(close) {
            return Ok(out);
        }
        loop {
            let t = self.ty()?;
            out.push(self.constant(&t, f)?);
            if self.eat(close) {
                return Ok(out);
            }
            self.expect(',')?;
        }
    }

    /// A value of type `ty`: a local, an argument, or a constant.
    fn value(&mut self, ty: &Type, f: &mut FnState) -> Res<Value> {
        if *ty == Type::Metadata {
            // A metadata operand (!9, !{...}, !"..."): nothing kernels use.
            if self.eat('!') && self.punct('{') {
                let mut depth = 0;
                while let Ok(t) = self.next() {
                    match t {
                        Tok::Punct('{') => depth += 1,
                        Tok::Punct('}') => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                self.next()?;
            }
            return Ok(Value::Const(Const::Undef));
        }
        if let Some(Tok::Local(n)) = self.peek() {
            self.i += 1;
            return Ok(f.named(n, ty));
        }
        Ok(Value::Const(self.constant(ty, f)?))
    }

    /// A type then a value.
    fn operand(&mut self, f: &mut FnState) -> Res<Operand> {
        let ty = self.ty()?;
        self.skip_param_attrs()?;
        let v = self.value(&ty, f)?;
        Ok(Operand { ty, v })
    }

    fn label(&mut self, f: &mut FnState) -> Res<usize> {
        if !self.eat_word("label") {
            bail!("expected a label, found {:?}", self.peek());
        }
        match self.next()? {
            Tok::Local(n) => Ok(f.block(n)),
            t => bail!("expected a label, found {t:?}"),
        }
    }

    /// Skips the rest of an instruction: metadata attachments, alignment,
    /// ordering, attributes.
    fn skip_to_line_end(&mut self, line_end: usize) {
        self.i = line_end;
    }
}

/// A function's names while it's being parsed.
#[derive(Default)]
struct FnState {
    values: HashMap<String, usize>,
    types: Vec<Type>,
    args: HashMap<String, usize>,
    blocks: HashMap<String, usize>,
    /// Block numbers in the order blocks are defined.
    defined: Vec<usize>,
    next_block: usize,
}

impl FnState {
    fn named(&mut self, n: &str, ty: &Type) -> Value {
        if let Some(&a) = self.args.get(n) {
            return Value::Arg(a);
        }
        let next = self.types.len();
        let id = *self.values.entry(n.to_string()).or_insert(next);
        if id == next {
            self.types.push(ty.clone());
        }
        Value::Local(id)
    }

    fn block(&mut self, n: &str) -> usize {
        let next = self.next_block;
        let id = *self.blocks.entry(n.to_string()).or_insert(next);
        if id == next {
            self.next_block += 1;
        }
        id
    }
}

pub fn parse(text: &str) -> Res<Module> {
    let mut m = Module::default();
    // Work line by line for the module's structure (each instruction is a
    // line), and token by token within lines.
    let lines: Vec<&str> = text.lines().collect();
    let mut n = 0;
    while n < lines.len() {
        let line = lines[n].trim();
        n += 1;
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with("define ") {
            let mut body = vec![line];
            while n < lines.len() && lines[n].trim() != "}" {
                body.push(lines[n]);
                n += 1;
            }
            n += 1;
            m.functions.push(function(&body).map_err(|e| format!("{e} (in {})", line.split('(').next().unwrap_or(line)))?);
        } else if line.starts_with('%') && line.contains("= type") {
            let toks = lex(line)?;
            let mut p = Parser { t: &toks, i: 0 };
            let Tok::Local(name) = p.next()? else { bail!("bad type definition: {line}") };
            p.expect('=')?;
            p.eat_word("type");
            if p.eat_word("opaque") {
                continue;
            }
            let t = p.ty()?;
            m.types.insert(name.clone(), t);
        } else if line.starts_with('@') {
            m.globals.push(global(line)?);
        } else if let Some(rest) = line.strip_prefix('!') {
            // !N = !{...}; named metadata (!opencl.ocl.version) doesn't matter.
            let Some((num, body)) = rest.split_once(" = ") else { continue };
            let Ok(num) = num.parse::<usize>() else { continue };
            let body = body.trim_start_matches("distinct ");
            let Some(body) = body.strip_prefix("!{").and_then(|b| b.strip_suffix('}')) else { continue };
            m.metadata.insert(num, meta_items(body));
            m.metadata_text.insert(num, body.to_string());
        }
    }
    Ok(m)
}

fn meta_items(body: &str) -> Vec<MetaItem> {
    let Ok(toks) = lex(body) else { return Vec::new() };
    let mut items = Vec::new();
    let mut p = Parser { t: &toks, i: 0 };
    while p.peek().is_some() {
        let item = match p.next().unwrap() {
            Tok::Str(s) => MetaItem::Str(String::from_utf8_lossy(s).into_owned()),
            Tok::Meta(n) => n.parse().map(MetaItem::Node).unwrap_or(MetaItem::Other),
            Tok::Word(_) => match p.next() {
                Ok(Tok::Int(v)) => MetaItem::Int(*v as i64),
                _ => MetaItem::Other,
            },
            _ => MetaItem::Other,
        };
        items.push(item);
        while p.peek().is_some() && !p.eat(',') {
            p.i += 1;
        }
    }
    items
}

fn global(line: &str) -> Res<Global> {
    let toks = lex(line)?;
    let mut p = Parser { t: &toks, i: 0 };
    let Tok::Global(name) = p.next()? else { bail!("bad global: {line}") };
    p.expect('=')?;
    let mut space = 0;
    let constant;
    loop {
        match p.next()? {
            Tok::Word(w) if w == "addrspace" => {
                p.expect('(')?;
                space = p.int()? as u32;
                p.expect(')')?;
            }
            Tok::Word(w) if w == "global" || w == "constant" => {
                constant = w == "constant";
                break;
            }
            Tok::Word(_) => {}
            t => bail!("unexpected {t:?} in global {line}"),
        }
    }
    let ty = p.ty()?;
    let mut f = FnState::default();
    let init = if matches!(p.peek(), Some(Tok::Punct(',')) | None) { None } else { Some(p.constant(&ty, &mut f)?) };
    let mut align = 1;
    while p.peek().is_some() {
        if p.eat_word("align") {
            align = p.int()? as u32;
        } else {
            p.i += 1;
        }
    }
    Ok(Global { name: name.clone(), ty, space, constant, init, align })
}

fn function(lines: &[&str]) -> Res<Function> {
    let head = lex(lines[0])?;
    let mut p = Parser { t: &head, i: 0 };
    let mut f = FnState::default();
    p.eat_word("define");
    let mut kernel = false;
    // Linkage, calling convention and return attributes, then the type.
    let ret = loop {
        match p.peek() {
            Some(Tok::Word(w)) if w == "spir_kernel" => {
                kernel = true;
                p.i += 1;
            }
            Some(Tok::Word(w))
                if [
                    "dso_local", "internal", "private", "spir_func", "local_unnamed_addr", "unnamed_addr", "hidden", "protected", "linkonce_odr",
                    "weak_odr", "available_externally", "fastcc", "ccc",
                ]
                .contains(&w.as_str()) || PARAM_ATTRS.contains(&w.as_str()) =>
            {
                p.i += 1;
                p.skip_group();
            }
            _ => break p.ty()?,
        }
    };
    let Tok::Global(name) = p.next()? else { bail!("expected a function name") };
    p.expect('(')?;
    let mut params = Vec::new();
    let mut byval = HashMap::new();
    if !p.eat(')') {
        loop {
            let t = p.ty()?;
            if let Some(b) = p.param_attrs()? {
                byval.insert(params.len(), b);
            }
            if let Some(Tok::Local(n)) = p.peek() {
                f.args.insert(n.clone(), params.len());
                p.i += 1;
            } else {
                f.args.insert(params.len().to_string(), params.len());
            }
            params.push(t);
            if p.eat(')') {
                break;
            }
            p.expect(',')?;
        }
    }
    // Unnamed values and blocks are numbered from after the arguments.
    let mut metadata = HashMap::new();
    while let Some(t) = p.peek() {
        p.i += 1;
        if let Tok::Meta(k) = t
            && let Some(Tok::Meta(v)) = p.peek()
            && let Ok(v) = v.parse()
        {
            metadata.insert(k.clone(), v);
            p.i += 1;
        }
    }
    let mut blocks: Vec<Option<Block>> = Vec::new();
    let mut current: Option<(usize, Vec<Inst>)> = None;
    let first = params.len().to_string();
    // A switch's cases are on lines of their own, up to "]".
    let mut joined: Vec<String> = Vec::new();
    let mut open = false;
    for line in &lines[1..] {
        let t = line.split(" ; ").next().unwrap_or(line).trim();
        if open {
            joined.last_mut().unwrap().push_str(&format!(" {t}"));
            open = t != "]";
        } else {
            joined.push(t.to_string());
            open = t.starts_with("switch ") && t.ends_with('[');
        }
    }
    for trimmed in &joined {
        let trimmed = trimmed.as_str();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }
        let toks = lex(trimmed)?;
        // A label: "name:" (any comment after it is gone).
        if toks.len() == 2 && toks[1] == Tok::Punct(':') {
            let n = match &toks[0] {
                Tok::Word(w) => w.clone(),
                Tok::Int(v) => v.to_string(),
                Tok::Str(s) => String::from_utf8_lossy(s).into_owned(),
                t => bail!("bad label {t:?}"),
            };
            if current.is_some() {
                bail!("block before {n} has no terminator");
            }
            let id = f.block(&n);
            f.defined.push(id);
            current = Some((id, Vec::new()));
            continue;
        }
        if current.is_none() {
            // The entry block, unlabelled: numbered after the arguments.
            let id = f.block(if f.defined.is_empty() { &first } else { "entry" });
            f.defined.push(id);
            current = Some((id, Vec::new()));
        }
        let (id, insts) = current.as_mut().unwrap();
        match instruction(&toks, &mut f).map_err(|e| format!("{e}: {trimmed}"))? {
            Line::Inst(i) => insts.push(i),
            Line::Term(t) => {
                let id = *id;
                let insts = std::mem::take(insts);
                if blocks.len() <= id {
                    blocks.resize(id + 1, None);
                }
                blocks[id] = Some(Block { insts, term: t });
                current = None;
            }
        }
    }
    // Renumber blocks in definition order (the entry first).
    let order = f.defined.clone();
    let mut renumber = vec![usize::MAX; f.next_block];
    for (new, &old) in order.iter().enumerate() {
        renumber[old] = new;
    }
    let mut out = Vec::new();
    for &old in &order {
        let mut b = blocks.get_mut(old).and_then(Option::take).ok_or("a block without a terminator")?;
        let map = |x: usize| -> Res<usize> { renumber.get(x).copied().filter(|&v| v != usize::MAX).ok_or_else(|| "branch to an undefined block".to_string()) };
        b.term = match b.term {
            Term::Br(t) => Term::Br(map(t)?),
            Term::CondBr(c, t, e) => Term::CondBr(c, map(t)?, map(e)?),
            Term::Switch(v, d, cases) => Term::Switch(v, map(d)?, cases.into_iter().map(|(c, t)| Ok((c, map(t)?))).collect::<Res<_>>()?),
            t => t,
        };
        for i in &mut b.insts {
            if let Op::Phi(incoming) = &mut i.op {
                for (_, blk) in incoming.iter_mut() {
                    *blk = map(*blk)?;
                }
            }
        }
        out.push(b);
    }
    Ok(Function { name: name.clone(), ret, params, kernel, blocks: out, values: f.types, metadata, byval })
}

enum Line {
    Inst(Inst),
    Term(Term),
}

fn instruction(toks: &[Tok], f: &mut FnState) -> Res<Line> {
    let mut p = Parser { t: toks, i: 0 };
    let end = toks.len();
    let mut result = None;
    if let (Some(Tok::Local(n)), Some(Tok::Punct('='))) = (p.peek(), p.peek_at(1)) {
        result = Some(n.clone());
        p.i += 2;
    }
    while matches!(p.peek(), Some(Tok::Word(w)) if w == "tail" || w == "musttail" || w == "notail") {
        p.i += 1;
    }
    let Tok::Word(op) = p.next()? else { bail!("expected an instruction") };
    let op = op.as_str();
    let inst = |p: &mut Parser, f: &mut FnState, ty: Type, op: Op| -> Res<Line> {
        p.skip_to_line_end(end);
        let result = match &result {
            Some(n) => match f.named(n, &ty) {
                Value::Local(id) => {
                    f.types[id] = ty.clone();
                    Some(id)
                }
                _ => bail!("an instruction redefines an argument"),
            },
            None => None,
        };
        Ok(Line::Inst(Inst { result, ty, op }))
    };
    // Flags after the opcode.
    let flags = |p: &mut Parser| {
        while let Some(Tok::Word(w)) = p.peek() {
            if ["nuw", "nsw", "exact", "disjoint", "nneg", "samesign", "inbounds", "nusw", "fast", "nnan", "ninf", "nsz", "arcp", "contract", "afn", "reassoc", "tail", "musttail", "notail", "volatile"]
                .contains(&w.as_str())
            {
                p.i += 1;
            } else {
                break;
            }
        }
    };
    flags(&mut p);
    match op {
        "add" | "sub" | "mul" | "udiv" | "sdiv" | "urem" | "srem" | "shl" | "lshr" | "ashr" | "and" | "or" | "xor" | "fadd" | "fsub" | "fmul"
        | "fdiv" | "frem" => {
            flags(&mut p);
            let a = p.operand(f)?;
            p.expect(',')?;
            let b = p.value(&a.ty, f)?;
            let ty = a.ty.clone();
            let b = Operand { ty: ty.clone(), v: b };
            inst(&mut p, f, ty, Op::Bin(op.to_string(), a, b))
        }
        "fneg" => {
            flags(&mut p);
            let a = p.operand(f)?;
            inst(&mut p, f, a.ty.clone(), Op::FNeg(a))
        }
        "icmp" | "fcmp" => {
            flags(&mut p);
            let Tok::Word(pred) = p.next()? else { bail!("expected a predicate") };
            let a = p.operand(f)?;
            p.expect(',')?;
            let b = Operand { ty: a.ty.clone(), v: p.value(&a.ty, f)? };
            let ty = match &a.ty {
                Type::Vector(n, _) => Type::Vector(*n, Box::new(Type::Int(1))),
                _ => Type::Int(1),
            };
            let o = if op == "icmp" { Op::ICmp(pred.clone(), a, b) } else { Op::FCmp(pred.clone(), a, b) };
            inst(&mut p, f, ty, o)
        }
        "select" => {
            flags(&mut p);
            let c = p.operand(f)?;
            p.expect(',')?;
            let a = p.operand(f)?;
            p.expect(',')?;
            let b = p.operand(f)?;
            inst(&mut p, f, a.ty.clone(), Op::Select(c, a, b))
        }
        "zext" | "sext" | "trunc" | "fptosi" | "fptoui" | "sitofp" | "uitofp" | "fptrunc" | "fpext" | "bitcast" | "ptrtoint" | "inttoptr"
        | "addrspacecast" => {
            flags(&mut p);
            let a = p.operand(f)?;
            if !p.eat_word("to") {
                bail!("expected 'to'");
            }
            let ty = p.ty()?;
            inst(&mut p, f, ty, Op::Cast(op.to_string(), a))
        }
        "getelementptr" => {
            flags(&mut p);
            let src = p.ty()?;
            p.expect(',')?;
            let base = p.operand(f)?;
            let mut idx = Vec::new();
            while p.eat(',') {
                if p.eat_word("inrange") {
                    p.skip_group();
                }
                if matches!(p.peek(), Some(Tok::Meta(_))) {
                    break;
                }
                idx.push(p.operand(f)?);
            }
            let ty = if let Some(Operand { ty: Type::Vector(n, _), .. }) = idx.iter().find(|o| matches!(o.ty, Type::Vector(..))) {
                Type::Vector(*n, Box::new(base.ty.clone()))
            } else {
                base.ty.clone()
            };
            inst(&mut p, f, ty, Op::Gep(src, base, idx))
        }
        "load" => {
            flags(&mut p);
            p.eat_word("atomic");
            flags(&mut p);
            let ty = p.ty()?;
            p.expect(',')?;
            let ptr = p.operand(f)?;
            let align = align_of(&p);
            inst(&mut p, f, ty, Op::Load(ptr, align))
        }
        "store" => {
            flags(&mut p);
            p.eat_word("atomic");
            flags(&mut p);
            let v = p.operand(f)?;
            p.expect(',')?;
            let ptr = p.operand(f)?;
            let align = align_of(&p);
            inst(&mut p, f, Type::Void, Op::Store(v, ptr, align))
        }
        "alloca" => {
            let ty = p.ty()?;
            let mut count = 1;
            let mut align = 1;
            while p.eat(',') {
                if p.eat_word("align") {
                    align = p.int()? as u32;
                } else if p.word("addrspace") {
                    break;
                } else {
                    let o = p.operand(f)?;
                    match o.v {
                        Value::Const(Const::Int(n)) => count = n,
                        _ => bail!("a variable-length alloca"),
                    }
                }
            }
            inst(&mut p, f, Type::Ptr(0), Op::Alloca(ty, count, align))
        }
        "phi" => {
            flags(&mut p);
            let ty = p.ty()?;
            let mut incoming = Vec::new();
            loop {
                p.expect('[')?;
                let v = p.value(&ty, f)?;
                p.expect(',')?;
                let Tok::Local(b) = p.next()? else { bail!("expected a block") };
                let b = f.block(b);
                p.expect(']')?;
                incoming.push((Operand { ty: ty.clone(), v }, b));
                if !p.eat(',') || matches!(p.peek(), Some(Tok::Meta(_))) {
                    break;
                }
            }
            inst(&mut p, f, ty, Op::Phi(incoming))
        }
        "call" => {
            flags(&mut p);
            // Calling convention and return attributes, then the type.
            while let Some(Tok::Word(w)) = p.peek() {
                if w == "spir_func" || w == "fastcc" || w == "ccc" || PARAM_ATTRS.contains(&w.as_str()) {
                    p.i += 1;
                    p.skip_group();
                    if matches!(p.peek(), Some(Tok::Int(_))) {
                        p.i += 1;
                    }
                } else {
                    break;
                }
            }
            let ty = p.ty()?;
            let callee = match p.next()? {
                Tok::Global(n) => n.clone(),
                t => bail!("an indirect call ({t:?})"),
            };
            p.expect('(')?;
            let mut args = Vec::new();
            if !p.eat(')') {
                loop {
                    args.push(p.operand(f)?);
                    if p.eat(')') {
                        break;
                    }
                    p.expect(',')?;
                }
            }
            inst(&mut p, f, ty, Op::Call(callee, args))
        }
        "extractelement" => {
            let v = p.operand(f)?;
            p.expect(',')?;
            let i = p.operand(f)?;
            let ty = v.ty.scalar().clone();
            inst(&mut p, f, ty, Op::ExtractElement(v, i))
        }
        "insertelement" => {
            let v = p.operand(f)?;
            p.expect(',')?;
            let e = p.operand(f)?;
            p.expect(',')?;
            let i = p.operand(f)?;
            inst(&mut p, f, v.ty.clone(), Op::InsertElement(v, e, i))
        }
        "shufflevector" => {
            let a = p.operand(f)?;
            p.expect(',')?;
            let b = p.operand(f)?;
            p.expect(',')?;
            let mt = p.ty()?;
            let mask = match p.constant(&mt, f)? {
                Const::Vector(items) => items.iter().map(|c| if let Const::Int(n) = c { *n as i64 } else { -1 }).collect(),
                Const::Zero => vec![0; mt.lanes() as usize],
                Const::Undef => vec![-1; mt.lanes() as usize],
                c => bail!("unexpected shuffle mask {c:?}"),
            };
            let ty = Type::Vector(mt.lanes(), Box::new(a.ty.scalar().clone()));
            inst(&mut p, f, ty, Op::Shuffle(a, b, mask))
        }
        "extractvalue" => {
            let v = p.operand(f)?;
            let mut idx = Vec::new();
            while p.eat(',') {
                if matches!(p.peek(), Some(Tok::Meta(_))) {
                    break;
                }
                idx.push(p.int()? as u32);
            }
            // The member's type, known from the aggregate's.
            let mut ty = v.ty.clone();
            for &k in &idx {
                ty = match ty {
                    Type::Struct(fields, _) => fields.get(k as usize).cloned().ok_or("extractvalue out of range")?,
                    Type::Array(_, e) => *e,
                    t => bail!("extractvalue from {t:?}"),
                };
            }
            inst(&mut p, f, ty, Op::ExtractValue(v, idx))
        }
        "insertvalue" => {
            let a = p.operand(f)?;
            p.expect(',')?;
            let v = p.operand(f)?;
            let mut idx = Vec::new();
            while p.eat(',') {
                idx.push(p.int()? as u32);
            }
            inst(&mut p, f, a.ty.clone(), Op::InsertValue(a, v, idx))
        }
        "atomicrmw" => {
            flags(&mut p);
            let Tok::Word(kind) = p.next()? else { bail!("expected an atomic operation") };
            let ptr = p.operand(f)?;
            p.expect(',')?;
            let v = p.operand(f)?;
            let ty = v.ty.clone();
            inst(&mut p, f, ty, Op::AtomicRmw(kind.clone(), ptr, v))
        }
        "cmpxchg" => {
            flags(&mut p);
            p.eat_word("weak");
            flags(&mut p);
            let ptr = p.operand(f)?;
            p.expect(',')?;
            let cmp = p.operand(f)?;
            p.expect(',')?;
            let new = p.operand(f)?;
            let ty = Type::Struct(vec![cmp.ty.clone(), Type::Int(1)], false);
            inst(&mut p, f, ty, Op::CmpXchg(ptr, cmp, new))
        }
        "fence" => inst(&mut p, f, Type::Void, Op::Fence),
        "freeze" => {
            let a = p.operand(f)?;
            inst(&mut p, f, a.ty.clone(), Op::Freeze(a))
        }
        "br" => {
            if p.word("label") {
                return Ok(Line::Term(Term::Br(p.label(f)?)));
            }
            let c = p.operand(f)?;
            p.expect(',')?;
            let t = p.label(f)?;
            p.expect(',')?;
            let e = p.label(f)?;
            Ok(Line::Term(Term::CondBr(c, t, e)))
        }
        "switch" => {
            let v = p.operand(f)?;
            p.expect(',')?;
            let d = p.label(f)?;
            p.expect('[')?;
            let mut cases = Vec::new();
            while !p.eat(']') {
                let c = p.operand(f)?;
                let Value::Const(Const::Int(n)) = c.v else { bail!("a switch case that isn't a constant") };
                p.expect(',')?;
                cases.push((n, p.label(f)?));
            }
            Ok(Line::Term(Term::Switch(v, d, cases)))
        }
        "ret" => {
            if p.eat_word("void") {
                return Ok(Line::Term(Term::Ret(None)));
            }
            Ok(Line::Term(Term::Ret(Some(p.operand(f)?))))
        }
        "unreachable" => Ok(Line::Term(Term::Unreachable)),
        op => bail!("unsupported instruction {op}"),
    }
}

/// An instruction's `align N`, if it has one.
fn align_of(p: &Parser) -> u32 {
    let t = &p.t[p.i..];
    for w in t.windows(2) {
        if let [Tok::Word(a), Tok::Int(n)] = w
            && a == "align"
        {
            return *n as u32;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADD: &str = r#"
target triple = "spir"
%struct.pair = type { i8, i32 }
@tbl = internal addrspace(2) constant [3 x float] [float 1.000000e+00, float 0x3FF8000000000000, float -2.500000e+00], align 4

define dso_local spir_kernel void @add(ptr addrspace(1) noundef readonly align 4 captures(none) %0, ptr addrspace(1) noundef readonly align 4 captures(none) %1, ptr addrspace(1) noundef writeonly align 4 captures(none) %2, i32 noundef %3) local_unnamed_addr #0 !kernel_arg_addr_space !4 !kernel_arg_type !6 {
  %5 = tail call spir_func i32 @_Z13get_global_idj(i32 noundef 0) #7
  %6 = icmp slt i32 %5, %3
  br i1 %6, label %7, label %14

7:                                                ; preds = %4
  %8 = getelementptr inbounds float, ptr addrspace(1) %0, i32 %5
  %9 = load float, ptr addrspace(1) %8, align 4, !tbaa !8
  %10 = getelementptr inbounds float, ptr addrspace(1) %1, i32 %5
  %11 = load float, ptr addrspace(1) %10, align 4, !tbaa !8
  %12 = fadd float %9, %11
  %13 = getelementptr inbounds float, ptr addrspace(1) %2, i32 %5
  store float %12, ptr addrspace(1) %13, align 4, !tbaa !8
  br label %14

14:                                               ; preds = %4, %7
  ret void
}

!4 = !{i32 1, i32 1, i32 1, i32 0}
!6 = !{!"float*", !"float*", !"float*", !"int"}
"#;

    #[test]
    fn a_kernel_parses() {
        let m = parse(ADD).unwrap();
        let f = m.function("add").unwrap();
        assert!(f.kernel);
        assert_eq!(f.params, [Type::Ptr(1), Type::Ptr(1), Type::Ptr(1), Type::Int(32)]);
        assert_eq!(f.blocks.len(), 3);
        assert_eq!(f.blocks[0].term, Term::CondBr(Operand { ty: Type::Int(1), v: Value::Local(1) }, 1, 2));
        assert!(matches!(&f.blocks[1].insts[4].op, Op::Bin(o, ..) if o == "fadd"));
        assert!(matches!(&f.blocks[1].insts[6].op, Op::Store(_, _, 4)));
        assert_eq!(m.meta_strings(f.metadata["kernel_arg_type"]), ["float*", "float*", "float*", "int"]);
        assert_eq!(m.meta_ints(f.metadata["kernel_arg_addr_space"]), [1, 1, 1, 0]);
        let g = &m.globals[0];
        assert_eq!((g.space, g.constant), (2, true));
        assert_eq!(g.init, Some(Const::Array(vec![Const::Float(1.0), Const::Float(1.5), Const::Float(-2.5)])));
        assert_eq!(m.layout(&Type::Named("struct.pair".into())).unwrap(), (8, 4));
        assert_eq!(m.field_offset(&Type::Named("struct.pair".into()), 1).unwrap(), 4);
        assert_eq!(m.layout(&Type::Vector(3, Box::new(Type::Float))).unwrap(), (16, 16));
    }

    #[test]
    fn loops_phis_and_vectors_parse() {
        let ir = r#"
define dso_local spir_kernel void @k(ptr addrspace(3) noundef align 4 %0, <4 x float> noundef %1) {
  %3 = tail call spir_func i32 @_Z14get_local_sizej(i32 noundef 0) #7
  br label %4

4:
  %5 = phi i32 [ %6, %4 ], [ %3, %2 ]
  %6 = lshr i32 %5, 1
  %7 = extractelement <4 x float> %1, i64 2
  %8 = insertelement <4 x float> <float poison, float poison, float poison, float 0.000000e+00>, float %7, i64 0
  %9 = shufflevector <4 x float> %8, <4 x float> poison, <2 x i32> <i32 0, i32 3>
  %10 = icmp ult i32 %5, 4
  br i1 %10, label %11, label %4

11:
  ret void
}
"#;
        let m = parse(ir).unwrap();
        let f = &m.functions[0];
        assert_eq!(f.blocks.len(), 3);
        let Op::Phi(inc) = &f.blocks[1].insts[0].op else { panic!() };
        assert_eq!(inc.iter().map(|(_, b)| *b).collect::<Vec<_>>(), [1, 0]);
        assert_eq!(f.blocks[1].insts[4].ty, Type::Vector(2, Box::new(Type::Float)));
        assert!(matches!(&f.blocks[1].insts[4].op, Op::Shuffle(_, _, m) if m == &[0, 3]));
    }
}
