//! The lifter's LLVM IR compiled straight to WebAssembly, for code a
//! program generates at run time (`--raw`) and pages translated again
//! (`--page`): the program waits for these, and clang and wasm-ld take a
//! tenth of a second or more even for a few blocks. The lifter's IR is a
//! small, regular subset of LLVM's (see lift/): scalar integers and
//! floats, loads and stores off computed addresses, compares and selects,
//! branches, a switch, calls into the support code and guaranteed tail
//! calls between region functions. This compiles that subset; a module
//! using anything else (vectors, i128, half precision, some intrinsics)
//! is reported unsupported, and the caller compiles it with clang.
//!
//! The output is the side module the runtime loads (docs/abi.md,
//! "Libraries"), shaped as wasm-ld makes it: a `dylink.0` section, the
//! shared memory, the function table, `__memory_base`, `__table_base`,
//! `__wasm_env_state` and the support code's functions imported from
//! `env`, a `GOT.func` global per region function (its table slot), and
//! the region table (`dispatch_addrs`, `dispatch_fns`, `dispatch_count`)
//! exported as offsets from `__memory_base`. There are no data segments:
//! `__wasm_apply_data_relocs`, which the runtime runs once, writes the
//! region table and the fatal messages' strings.
//!
//! Code generation is simple and direct. Every SSA value is a local; `i1`
//! to `i32` values are `i32`s kept zero-extended, `i64` and pointers `i64`.
//! A function's basic blocks are cases of a loop around a `br_table` on a
//! block-number local (a branch to the next block falls through); phis
//! become copies on the edges into their block.

use std::collections::{BTreeMap, HashMap};

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    GlobalSection, GlobalType, ImportSection, Instruction as I, MemArg, MemoryType, Module, RefType, TableType, TypeSection,
    ValType,
};

type Res<T> = Result<T, String>;

macro_rules! unsupported {
    ($($arg:tt)*) => { return Err(format!($($arg)*)) };
}

/// Compiles the lifter's chunks (see lift::lift) to a side module, or says
/// why it can't.
pub fn compile(chunks: &[String]) -> Res<Vec<u8>> {
    let mut m = Parsed::default();
    for c in chunks {
        m.parse(c)?;
    }
    m.module()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    I1,
    I8,
    I16,
    I32,
    I64,
    Ptr,
    F32,
    F64,
    /// A SIMD/FP register (the lifter's i128), as a v128.
    I128,
}

impl Ty {
    fn parse(s: &str) -> Option<Ty> {
        Some(match s.trim() {
            "i1" => Ty::I1,
            "i8" => Ty::I8,
            "i16" => Ty::I16,
            "i32" => Ty::I32,
            "i64" => Ty::I64,
            "ptr" => Ty::Ptr,
            "float" => Ty::F32,
            "double" => Ty::F64,
            "i128" => Ty::I128,
            _ => return None,
        })
    }
    fn val(self) -> ValType {
        match self {
            Ty::I1 | Ty::I8 | Ty::I16 | Ty::I32 => ValType::I32,
            Ty::I64 | Ty::Ptr => ValType::I64,
            Ty::F32 => ValType::F32,
            Ty::F64 => ValType::F64,
            Ty::I128 => ValType::V128,
        }
    }
    fn bits(self) -> u32 {
        match self {
            Ty::I1 => 1,
            Ty::I8 => 8,
            Ty::I16 => 16,
            Ty::I32 | Ty::F32 => 32,
            Ty::I64 | Ty::Ptr | Ty::F64 => 64,
            Ty::I128 => 128,
        }
    }
    /// Narrower than its WebAssembly type (kept zero-extended).
    fn narrow(self) -> bool {
        matches!(self, Ty::I1 | Ty::I8 | Ty::I16)
    }
    fn wide(self) -> bool {
        self.val() == ValType::I64
    }
    fn float(self) -> bool {
        matches!(self, Ty::F32 | Ty::F64)
    }
}

/// A support-code function the module imports (from a `declare`).
#[derive(Clone)]
struct Import {
    params: Vec<ValType>,
    result: Option<ValType>,
}

#[derive(Default)]
struct Parsed {
    imports: BTreeMap<String, Import>,
    strings: BTreeMap<String, Vec<u8>>,
    /// Region functions: (name, body lines).
    functions: Vec<(String, Vec<String>)>,
}

impl Parsed {
    fn parse(&mut self, chunk: &str) -> Res<()> {
        let mut lines = chunk.lines();
        while let Some(line) = lines.next() {
            if let Some(rest) = line.strip_prefix("declare ") {
                // declare RET @name(ARGS)
                let (ret, rest) = rest.split_once(" @").ok_or("declare")?;
                let (name, args) = rest.split_once('(').ok_or("declare")?;
                if name.starts_with("llvm.") || name.starts_with("R_") {
                    continue;
                }
                let args = &args[..args.find(')').ok_or("declare")?];
                let mut params = Vec::new();
                for a in split_top(args) {
                    let a = a.trim();
                    if a.is_empty() {
                        continue;
                    }
                    params.push(Ty::parse(a.split_whitespace().next().unwrap_or("")).ok_or_else(|| format!("declare {a}"))?.val());
                }
                let ret = ret.split_whitespace().last().unwrap_or("void");
                let result = if ret == "void" { None } else { Some(Ty::parse(ret).ok_or_else(|| format!("declare {ret}"))?.val()) };
                self.imports.insert(name.to_string(), Import { params, result });
            } else if line.starts_with("@S_") {
                // @S_x = private constant [N x i8] c"..."
                let (name, rest) = line.split_once(" = ").ok_or("string")?;
                let (_, text) = rest.split_once("c\"").ok_or("string")?;
                let text = text.strip_suffix('"').ok_or("string")?;
                self.strings.insert(name[1..].to_string(), unescape(text)?);
            } else if let Some(rest) = line.strip_prefix("define void @") {
                let (name, _) = rest.split_once('(').ok_or("define")?;
                let mut body = Vec::new();
                for l in lines.by_ref() {
                    if l == "}" {
                        break;
                    }
                    body.push(l.to_string());
                }
                self.functions.push((name.to_string(), body));
            }
        }
        Ok(())
    }

    fn module(&self) -> Res<Vec<u8>> {
        // The data the region table and strings take (see the module doc).
        let n = self.functions.len() as u64;
        let (addrs, fns, count) = (0, 8 * n, 16 * n);
        let mut string_at = HashMap::new();
        let mut size = count + 8;
        for (name, bytes) in &self.strings {
            string_at.insert(name.clone(), size);
            size = (size + bytes.len() as u64).next_multiple_of(8);
        }

        let mut types = Types::default();
        let region_ty = types.get(vec![ValType::I64], vec![]);
        let void_ty = types.get(vec![], vec![]);
        // Imports: memory, table, globals, functions, then GOT.func globals.
        let mut imports = ImportSection::new();
        imports.import(
            "env",
            "memory",
            EntityType::Memory(MemoryType { minimum: 1, maximum: Some(crate::link::MAX_PAGES), memory64: true, shared: true, page_size_log2: None }),
        );
        imports.import(
            "env",
            "__indirect_function_table",
            EntityType::Table(TableType { element_type: RefType::FUNCREF, table64: true, minimum: 0, maximum: None, shared: false }),
        );
        let global = |mutable| EntityType::Global(GlobalType { val_type: ValType::I64, mutable, shared: false });
        imports.import("env", "__memory_base", global(false));
        imports.import("env", "__table_base", global(false));
        imports.import("env", "__wasm_env_state", global(true));
        const MEMORY_BASE: u32 = 0;
        const STATE: u32 = 2;
        let mut func_index = HashMap::new();
        let mut import_sig = HashMap::new();
        for (name, imp) in &self.imports {
            let t = types.get(imp.params.clone(), imp.result.into_iter().collect());
            imports.import("env", name, EntityType::Function(t));
            func_index.insert(name.clone(), func_index.len() as u32);
            import_sig.insert(name.clone(), imp.clone());
        }
        let imported_funcs = func_index.len() as u32;
        let got_base = 3;
        for (k, (name, _)) in self.functions.iter().enumerate() {
            imports.import("GOT.func", name, global(true));
            func_index.insert(name.clone(), imported_funcs + k as u32);
        }
        let relocs_index = imported_funcs + n as u32;

        let ctx = Context { func_index: &func_index, import_sig: &import_sig, string_at: &string_at, region_ty, memory_base: MEMORY_BASE, state: STATE };
        let mut functions = FunctionSection::new();
        let mut code = CodeSection::new();
        let mut addresses = Vec::new();
        for (name, body) in &self.functions {
            let f = FuncGen::new(&ctx, body)?.compile().map_err(|e| format!("{name}: {e}"))?;
            functions.function(region_ty);
            code.function(&f);
            let addr = name.strip_prefix("R_").and_then(|a| u64::from_str_radix(a, 16).ok()).ok_or("region name")?;
            addresses.push(addr);
        }
        // __wasm_apply_data_relocs: the region table and the strings.
        functions.function(void_ty);
        let mut relocs = Function::new(vec![]);
        let store64 = |f: &mut Function, offset: u64, value: &[I]| {
            f.instruction(&I::GlobalGet(MEMORY_BASE));
            for i in value {
                f.instruction(i);
            }
            f.instruction(&I::I64Store(MemArg { offset, align: 3, memory_index: 0 }));
        };
        for (k, &a) in addresses.iter().enumerate() {
            store64(&mut relocs, addrs + 8 * k as u64, &[I::I64Const(a as i64)]);
            store64(&mut relocs, fns + 8 * k as u64, &[I::GlobalGet(got_base + k as u32)]);
        }
        store64(&mut relocs, count, &[I::I64Const(n as i64)]);
        for (name, bytes) in &self.strings {
            // Eight bytes at a time (the string's space is padded to eight).
            let at = string_at[name];
            for (k, chunk) in bytes.chunks(8).enumerate() {
                let mut word = [0u8; 8];
                word[..chunk.len()].copy_from_slice(chunk);
                store64(&mut relocs, at + 8 * k as u64, &[I::I64Const(i64::from_le_bytes(word))]);
            }
        }
        relocs.instruction(&I::End);
        code.function(&relocs);

        let mut globals = GlobalSection::new();
        let mut exports = ExportSection::new();
        let first_global = got_base + n as u32;
        for (k, (name, offset)) in [("dispatch_addrs", addrs), ("dispatch_fns", fns), ("dispatch_count", count)].into_iter().enumerate() {
            globals.global(GlobalType { val_type: ValType::I64, mutable: false, shared: false }, &ConstExpr::i64_const(offset as i64));
            exports.export(name, ExportKind::Global, first_global + k as u32);
        }
        for (name, _) in &self.functions {
            exports.export(name, ExportKind::Func, func_index[name]);
        }
        exports.export("__wasm_apply_data_relocs", ExportKind::Func, relocs_index);

        // dylink.0: WASM_DYLINK_MEM_INFO (size, alignment log2, table size
        // and alignment).
        let mut info = Vec::new();
        for v in [size, 3, 0, 0] {
            uleb(&mut info, v);
        }
        let mut dylink = vec![1u8];
        uleb(&mut dylink, info.len() as u64);
        dylink.extend(info);

        let mut module = Module::new();
        module.section(&CustomSection { name: "dylink.0".into(), data: dylink.into() });
        module.section(&types.section());
        module.section(&imports);
        module.section(&functions);
        module.section(&globals);
        module.section(&exports);
        module.section(&code);
        Ok(module.finish())
    }
}

#[derive(Default)]
struct Types {
    list: Vec<(Vec<ValType>, Vec<ValType>)>,
}

impl Types {
    fn get(&mut self, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        let key = (params, results);
        if let Some(k) = self.list.iter().position(|t| *t == key) {
            return k as u32;
        }
        self.list.push(key);
        self.list.len() as u32 - 1
    }
    fn section(&self) -> TypeSection {
        let mut s = TypeSection::new();
        for (p, r) in &self.list {
            s.ty().function(p.iter().copied(), r.iter().copied());
        }
        s
    }
}

struct Context<'a> {
    func_index: &'a HashMap<String, u32>,
    import_sig: &'a HashMap<String, Import>,
    string_at: &'a HashMap<String, u64>,
    region_ty: u32,
    memory_base: u32,
    state: u32,
}

/// One basic block: its label and lines.
struct Block {
    label: String,
    lines: Vec<String>,
}

struct FuncGen<'a> {
    ctx: &'a Context<'a>,
    blocks: Vec<Block>,
    index: HashMap<String, usize>,
    /// Phis by block: (destination, type, [(value, predecessor label)]).
    phis: HashMap<String, Vec<(String, Ty, Vec<(String, String)>)>>,
    /// SSA name → (local, type); local 0 is %pc.
    locals: HashMap<String, (u32, Ty)>,
    local_types: Vec<ValType>,
    /// cmpxchg results: name → (old value's local, success's local).
    pairs: HashMap<String, (u32, u32)>,
    /// Addresses left as expressions (a value plus a constant: a constant
    /// getelementptr, `add nuw` of a constant, inttoptr), computed where
    /// they're used, and folded into a load's or store's offset there, as
    /// LLVM would: name → (base value, offset).
    addr: HashMap<String, (String, i64)>,
    code: Vec<I<'static>>,
    /// The block-number local.
    label: u32,
    /// Constructs opened in the current block's code (if, block).
    extra: u32,
    current: usize,
}

impl<'a> FuncGen<'a> {
    fn new(ctx: &'a Context<'a>, body: &[String]) -> Res<Self> {
        let mut blocks: Vec<Block> = Vec::new();
        for line in body {
            if line.is_empty() || line.trim_start().starts_with(';') {
                continue;
            }
            if !line.starts_with(' ') && line.ends_with(':') {
                blocks.push(Block { label: line.trim_end_matches(':').to_string(), lines: Vec::new() });
            } else {
                let Some(b) = blocks.last_mut() else { unsupported!("code before a label") };
                b.lines.push(line.trim().to_string());
            }
        }
        let index = blocks.iter().enumerate().map(|(k, b)| (b.label.clone(), k)).collect();
        let mut g = FuncGen {
            ctx,
            blocks,
            index,
            phis: HashMap::new(),
            locals: HashMap::new(),
            local_types: Vec::new(),
            pairs: HashMap::new(),
            addr: HashMap::new(),
            code: Vec::new(),
            label: 0,
            extra: 0,
            current: 0,
        };
        g.locals.insert("pc".into(), (0, Ty::I64));
        g.label = g.new_local(ValType::I32);
        // Phis: their locals exist before any predecessor copies into them.
        let mut phis: HashMap<String, Vec<(String, Ty, Vec<(String, String)>)>> = HashMap::new();
        for b in &g.blocks {
            for l in &b.lines {
                if let Some((dest, rest)) = l.split_once(" = phi ") {
                    let (ty, incoming) = rest.split_once(' ').ok_or("phi")?;
                    let ty = Ty::parse(ty).ok_or_else(|| format!("phi {ty}"))?;
                    let mut list = Vec::new();
                    for part in split_top(incoming) {
                        let part = part.trim().trim_start_matches('[').trim_end_matches(']').trim();
                        let (v, pred) = part.split_once(',').ok_or("phi")?;
                        list.push((v.trim().to_string(), pred.trim().trim_start_matches('%').to_string()));
                    }
                    phis.entry(b.label.clone()).or_default().push((dest[1..].to_string(), ty, list));
                }
            }
        }
        for list in phis.values() {
            for (dest, ty, _) in list {
                g.local(dest, *ty);
            }
        }
        g.phis = phis;
        Ok(g)
    }

    fn new_local(&mut self, t: ValType) -> u32 {
        self.local_types.push(t);
        self.local_types.len() as u32 // local 0 is the parameter
    }

    /// The local for SSA value `name` (without `%`), made on first sight.
    fn local(&mut self, name: &str, ty: Ty) -> u32 {
        if let Some(&(l, _)) = self.locals.get(name) {
            return l;
        }
        let l = self.new_local(ty.val());
        self.locals.insert(name.to_string(), (l, ty));
        l
    }

    fn emit(&mut self, i: I<'static>) {
        self.code.push(i);
    }

    fn compile(mut self) -> Res<Function> {
        let n = self.blocks.len() as u32;
        if n == 0 {
            unsupported!("no blocks");
        }
        // loop { block_{n-1} { ... block_0 { br_table } code_0 } ... code_{n-1} }
        self.emit(I::I32Const(0));
        self.emit(I::LocalSet(self.label));
        self.emit(I::Loop(BlockType::Empty));
        for _ in 0..n {
            self.emit(I::Block(BlockType::Empty));
        }
        self.emit(I::LocalGet(self.label));
        let targets: Vec<u32> = (0..n).collect();
        self.emit(I::BrTable(targets.into(), 0));
        self.emit(I::End);
        for k in 0..self.blocks.len() {
            self.current = k;
            self.extra = 0;
            let lines = std::mem::take(&mut self.blocks[k].lines);
            let mut it = lines.iter().peekable();
            while let Some(line) = it.next() {
                self.line(line, &mut it)?;
            }
            if k + 1 < self.blocks.len() {
                self.emit(I::End);
            }
        }
        self.emit(I::End); // loop
        self.emit(I::Unreachable);
        self.emit(I::End);
        // A value used once, right where it's made, stays on the stack: a
        // `local.set x` and the following `local.get x`, x's only uses, go.
        let (mut sets, mut gets) = (HashMap::<u32, u32>::new(), HashMap::<u32, u32>::new());
        for i in &self.code {
            match i {
                I::LocalSet(l) | I::LocalTee(l) => *sets.entry(*l).or_default() += 1,
                I::LocalGet(l) => *gets.entry(*l).or_default() += 1,
                _ => {}
            }
        }
        let mut code = Vec::with_capacity(self.code.len());
        let mut it = std::mem::take(&mut self.code).into_iter().peekable();
        while let Some(i) = it.next() {
            if let I::LocalSet(l) = i
                && sets.get(&l) == Some(&1)
                && gets.get(&l) == Some(&1)
                && matches!(it.peek(), Some(I::LocalGet(g)) if *g == l)
            {
                it.next();
                continue;
            }
            code.push(i);
        }
        // Locals declared in runs of one type.
        let mut locals: Vec<(u32, ValType)> = Vec::new();
        for &t in &self.local_types {
            match locals.last_mut() {
                Some((n, last)) if *last == t => *n += 1,
                _ => locals.push((1, t)),
            }
        }
        let mut f = Function::new(locals);
        for i in &code {
            f.instruction(i);
        }
        Ok(f)
    }

    /// Depth of the loop from the current point.
    fn loop_depth(&self) -> u32 {
        self.blocks.len() as u32 - 1 - self.current as u32 + self.extra
    }

    /// Continues at block `target` (phi copies, then the dispatch), or falls
    /// into it if it's next.
    fn goto(&mut self, target: &str, fallthrough: bool) -> Res<()> {
        let t = *self.index.get(target).ok_or_else(|| format!("no block {target}"))?;
        let from = self.blocks[self.current].label.clone();
        if let Some(list) = self.phis.get(target).cloned() {
            // A parallel copy: every value first, then every destination.
            let mut dests = Vec::new();
            for (dest, ty, incoming) in &list {
                let Some((v, _)) = incoming.iter().find(|(_, p)| *p == from) else { unsupported!("phi without {from}") };
                self.value(v, *ty)?;
                dests.push(self.locals[dest].0);
            }
            for d in dests.into_iter().rev() {
                self.emit(I::LocalSet(d));
            }
        }
        if fallthrough && self.extra == 0 && t == self.current + 1 {
            return Ok(());
        }
        self.emit(I::I32Const(t as i32));
        self.emit(I::LocalSet(self.label));
        self.emit(I::Br(self.loop_depth()));
        Ok(())
    }

    /// Pushes a value of type `ty`: a local, a constant or a global's address.
    fn value(&mut self, v: &str, ty: Ty) -> Res<()> {
        let v = v.trim();
        if let Some((base, off)) = v.strip_prefix('%').and_then(|n| self.resolve(n)) {
            self.value(&base, Ty::I64)?;
            if off != 0 {
                self.emit(I::I64Const(off));
                self.emit(I::I64Add);
            }
            return Ok(());
        }
        if let Some(name) = v.strip_prefix('%') {
            let l = self.local(name, ty);
            self.emit(I::LocalGet(l));
            return Ok(());
        }
        if let Some(name) = v.strip_prefix('@') {
            let Some(&at) = self.ctx.string_at.get(name) else { unsupported!("global @{name}") };
            self.emit(I::GlobalGet(self.ctx.memory_base));
            self.emit(I::I64Const(at as i64));
            self.emit(I::I64Add);
            return Ok(());
        }
        match (ty, v) {
            (_, "poison" | "undef" | "zeroinitializer") => self.zero(ty),
            (Ty::I1, "true") => self.emit(I::I32Const(1)),
            (Ty::I1, "false") => self.emit(I::I32Const(0)),
            (Ty::Ptr, "null") => self.emit(I::I64Const(0)),
            (Ty::F32 | Ty::F64, _) => {
                let bits = if let Some(h) = v.strip_prefix("0x") {
                    if h.starts_with(['K', 'L', 'M', 'H', 'R']) {
                        unsupported!("float literal {v}");
                    }
                    f64::from_bits(u64::from_str_radix(h, 16).map_err(|_| format!("float {v}"))?)
                } else {
                    v.parse::<f64>().map_err(|_| format!("float {v}"))?
                };
                if ty == Ty::F32 {
                    self.emit(I::F32Const((bits as f32).into()));
                } else {
                    self.emit(I::F64Const(bits.into()));
                }
            }
            _ => {
                let n: i128 = v.parse().map_err(|_| format!("value {v}"))?;
                match ty {
                    Ty::I128 => self.emit(I::V128Const(n)),
                    Ty::I64 | Ty::Ptr => self.emit(I::I64Const(n as i64)),
                    t => {
                        let mask = if t.bits() == 32 { u32::MAX } else { (1u32 << t.bits()) - 1 };
                        self.emit(I::I32Const(((n as u32) & mask) as i32));
                    }
                }
            }
        }
        Ok(())
    }

    /// A deferred address (see `addr`) as the value it's based on and a
    /// constant offset, following chains of them.
    fn resolve(&self, name: &str) -> Option<(String, i64)> {
        let (base, off) = self.addr.get(name)?;
        match base.strip_prefix('%').and_then(|b| self.resolve(b)) {
            Some((root, more)) => Some((root, off + more)),
            None => Some((base.clone(), *off)),
        }
    }

    /// Pushes a load's or store's address operand (`ptr %x`), and returns
    /// the constant offset for the access itself.
    fn address(&mut self, p: &str) -> Res<u64> {
        let v = p.trim().strip_prefix("ptr ").ok_or_else(|| format!("address {p}"))?.trim();
        if let Some((base, off)) = v.strip_prefix('%').and_then(|n| self.resolve(n))
            && off >= 0
        {
            self.value(&base, Ty::I64)?;
            return Ok(off as u64);
        }
        self.value(v, Ty::Ptr)?;
        Ok(0)
    }

    /// Leaves `dest = op` as an address expression if it is one (see
    /// `addr`): inttoptr and ptrtoint, `add nuw` of a non-negative constant
    /// (guest addresses, which the lifter marks so), and a getelementptr
    /// with constant indices (guest registers).
    fn defer(&mut self, dest: &str, op: &str) -> bool {
        let found = if let Some(rest) = op.strip_prefix("inttoptr i64 ").or_else(|| op.strip_prefix("ptrtoint ptr ")) {
            rest.split_once(" to ").filter(|(v, _)| v.starts_with('%')).map(|(v, _)| (v.to_string(), 0))
        } else if let Some(rest) = op.strip_prefix("add nuw i64 ").or_else(|| op.strip_prefix("add nuw nsw i64 ")) {
            rest.split_once(", ").and_then(|(v, k)| {
                let k: i64 = k.trim().parse().ok()?;
                (v.starts_with('%') && k >= 0).then(|| (v.to_string(), k))
            })
        } else if let Some(rest) = op.strip_prefix("getelementptr ") {
            gep_constant(rest)
        } else {
            None
        };
        match found {
            Some(e) => {
                self.addr.insert(dest.to_string(), e);
                true
            }
            None => false,
        }
    }

    fn zero(&mut self, ty: Ty) {
        self.emit(match ty.val() {
            ValType::I64 => I::I64Const(0),
            ValType::V128 => I::V128Const(0),
            ValType::F32 => I::F32Const(0.0f32.into()),
            ValType::F64 => I::F64Const(0.0f64.into()),
            _ => I::I32Const(0),
        });
    }

    /// Masks a narrow integer back to its width.
    fn normalize(&mut self, ty: Ty) {
        if ty.narrow() {
            self.emit(I::I32Const(((1u32 << ty.bits()) - 1) as i32));
            self.emit(I::I32And);
        }
    }

    /// Sign-extends a narrow integer (on the stack) to 32 bits.
    fn sign_extend(&mut self, ty: Ty) {
        if ty.narrow() {
            let s = 32 - ty.bits() as i32;
            self.emit(I::I32Const(s));
            self.emit(I::I32Shl);
            self.emit(I::I32Const(s));
            self.emit(I::I32ShrS);
        }
    }

    /// A typed operand, "T v".
    fn typed(&mut self, s: &str) -> Res<Ty> {
        let (t, v) = s.trim().split_once(' ').ok_or_else(|| format!("operand {s}"))?;
        let ty = Ty::parse(t).ok_or_else(|| format!("type {t}"))?;
        self.value(v, ty)?;
        Ok(ty)
    }

    fn define(&mut self, dest: Option<&str>, ty: Ty) -> Res<()> {
        match dest {
            Some(d) => {
                let l = self.local(d, ty);
                self.emit(I::LocalSet(l));
            }
            None => self.emit(I::Drop),
        }
        Ok(())
    }

    fn line<'l>(&mut self, line: &str, rest: &mut std::iter::Peekable<impl Iterator<Item = &'l String>>) -> Res<()> {
        let line = strip_metadata(line);
        let (dest, op) = match line.split_once(" = ") {
            Some((d, o)) if d.starts_with('%') => (Some(&d[1..]), o),
            _ => (None, line.as_str()),
        };
        if let Some(d) = dest
            && self.defer(d, op)
        {
            return Ok(());
        }
        let (word, args) = op.split_once(' ').unwrap_or((op, ""));
        match word {
            "phi" => Ok(()),
            "br" => {
                if let Some(label) = args.strip_prefix("label %") {
                    return self.goto(label, true);
                }
                let parts = split_top(args);
                let [c, a, b] = parts.as_slice() else { unsupported!("br {args}") };
                self.typed(c)?;
                self.emit(I::If(BlockType::Empty));
                self.extra += 1;
                self.goto(a.trim().trim_start_matches("label %"), false)?;
                self.emit(I::Else);
                self.goto(b.trim().trim_start_matches("label %"), false)?;
                self.emit(I::End);
                self.extra -= 1;
                self.emit(I::Unreachable);
                Ok(())
            }
            "switch" => self.switch(args, rest),
            "ret" => {
                self.emit(I::Return);
                Ok(())
            }
            "unreachable" => {
                self.emit(I::Unreachable);
                Ok(())
            }
            "musttail" => {
                let call = args.strip_prefix("call void ").ok_or("musttail")?;
                let (callee, a) = call.split_once('(').ok_or("musttail")?;
                self.typed(a.trim_end_matches(')'))?;
                if let Some(name) = callee.strip_prefix('@') {
                    let Some(&f) = self.ctx.func_index.get(name) else { unsupported!("tail call to {name}") };
                    self.emit(I::ReturnCall(f));
                } else {
                    self.value(callee, Ty::Ptr)?;
                    self.emit(I::ReturnCallIndirect { type_index: self.ctx.region_ty, table_index: 0 });
                }
                if rest.peek().is_some_and(|l| l.as_str() == "ret void") {
                    rest.next();
                }
                Ok(())
            }
            "call" => self.call(dest, args),
            "store" => self.store(args),
            "load" => self.load(dest, args),
            "getelementptr" => self.gep(dest, args),
            "icmp" => self.icmp(dest, args),
            "fcmp" => self.fcmp(dest, args),
            "select" => {
                let parts = split_top(args);
                let [c, a, b] = parts.as_slice() else { unsupported!("select") };
                let ty = self.typed(a)?;
                self.typed(b)?;
                self.typed(c)?;
                self.emit(if ty == Ty::I128 { I::TypedSelect(ValType::V128) } else { I::Select });
                self.define(dest, ty)
            }
            "freeze" => {
                let ty = self.typed(args)?;
                self.define(dest, ty)
            }
            "extractvalue" => {
                let (pair, index) = args.rsplit_once(',').ok_or("extractvalue")?;
                let name = pair.rsplit_once(' ').map(|(_, n)| n).unwrap_or(pair).trim_start_matches('%');
                let Some(&(old, ok)) = self.pairs.get(name) else { unsupported!("extractvalue of {name}") };
                let (l, ty) = if index.trim() == "0" { (old, self.locals_ty(old)) } else { (ok, Ty::I1) };
                self.emit(I::LocalGet(l));
                self.define(dest, ty)
            }
            "cmpxchg" => self.cmpxchg(dest, args),
            "atomicrmw" => self.atomicrmw(dest, args),
            "fence" => {
                self.emit(I::AtomicFence);
                Ok(())
            }
            "fneg" => {
                let ty = self.typed(args)?;
                self.emit(if ty == Ty::F32 { I::F32Neg } else { I::F64Neg });
                self.define(dest, ty)
            }
            "zext" | "sext" | "trunc" | "bitcast" | "inttoptr" | "ptrtoint" | "fpext" | "fptrunc" | "sitofp" | "uitofp" | "fptosi"
            | "fptoui" => self.cast(dest, word, args),
            _ => {
                if let Some(ty) = self.binop(word, args)? {
                    return self.define(dest, ty);
                }
                unsupported!("{word}")
            }
        }
    }

    fn locals_ty(&self, l: u32) -> Ty {
        self.locals.values().find(|(x, _)| *x == l).map(|&(_, t)| t).unwrap_or(Ty::I64)
    }

    /// Integer and float arithmetic: `op [flags] T a, b`.
    fn binop(&mut self, op: &str, args: &str) -> Res<Option<Ty>> {
        let args = skip_flags(args);
        let parts = split_top(args);
        let [a, b] = parts.as_slice() else { return Ok(None) };
        let (t, a) = a.trim().split_once(' ').ok_or("binop")?;
        let Some(ty) = Ty::parse(t) else { unsupported!("{op} on {t}") };
        if ty == Ty::I128 {
            self.binop128(op, a, b)?;
            return Ok(Some(ty));
        }
        let signed = matches!(op, "ashr" | "sdiv" | "srem");
        self.value(a, ty)?;
        if signed {
            self.sign_extend(ty);
        }
        self.value(b, ty)?;
        if signed && op != "ashr" {
            self.sign_extend(ty);
        }
        let w = ty.wide();
        let i = match (op, ty.float(), ty) {
            ("add", false, _) => if w { I::I64Add } else { I::I32Add },
            ("sub", false, _) => if w { I::I64Sub } else { I::I32Sub },
            ("mul", false, _) => if w { I::I64Mul } else { I::I32Mul },
            ("and", false, _) => if w { I::I64And } else { I::I32And },
            ("or", false, _) => if w { I::I64Or } else { I::I32Or },
            ("xor", false, _) => if w { I::I64Xor } else { I::I32Xor },
            ("shl", false, _) => if w { I::I64Shl } else { I::I32Shl },
            ("lshr", false, _) => if w { I::I64ShrU } else { I::I32ShrU },
            ("ashr", false, _) => if w { I::I64ShrS } else { I::I32ShrS },
            ("udiv", false, _) => if w { I::I64DivU } else { I::I32DivU },
            ("sdiv", false, _) => if w { I::I64DivS } else { I::I32DivS },
            ("urem", false, _) => if w { I::I64RemU } else { I::I32RemU },
            ("srem", false, _) => if w { I::I64RemS } else { I::I32RemS },
            ("fadd", true, Ty::F32) => I::F32Add,
            ("fadd", true, _) => I::F64Add,
            ("fsub", true, Ty::F32) => I::F32Sub,
            ("fsub", true, _) => I::F64Sub,
            ("fmul", true, Ty::F32) => I::F32Mul,
            ("fmul", true, _) => I::F64Mul,
            ("fdiv", true, Ty::F32) => I::F32Div,
            ("fdiv", true, _) => I::F64Div,
            _ => return Ok(None),
        };
        self.emit(i);
        // Narrow results back to their width (shifts may bring in bits).
        if !ty.float() && !matches!(op, "and" | "or" | "xor" | "lshr" | "udiv" | "urem") {
            self.normalize(ty);
        }
        Ok(Some(ty))
    }

    fn icmp(&mut self, dest: Option<&str>, args: &str) -> Res<()> {
        let (pred, rest) = args.split_once(' ').ok_or("icmp")?;
        let parts = split_top(rest);
        let [a, b] = parts.as_slice() else { unsupported!("icmp") };
        let (t, a) = a.trim().split_once(' ').ok_or("icmp")?;
        let Some(ty) = Ty::parse(t) else { unsupported!("icmp on {t}") };
        if ty == Ty::I128 {
            unsupported!("icmp on i128");
        }
        let signed = pred.starts_with('s');
        self.value(a, ty)?;
        if signed {
            self.sign_extend(ty);
        }
        self.value(b, ty)?;
        if signed {
            self.sign_extend(ty);
        }
        let w = ty.wide();
        self.emit(match pred {
            "eq" => if w { I::I64Eq } else { I::I32Eq },
            "ne" => if w { I::I64Ne } else { I::I32Ne },
            "ugt" => if w { I::I64GtU } else { I::I32GtU },
            "uge" => if w { I::I64GeU } else { I::I32GeU },
            "ult" => if w { I::I64LtU } else { I::I32LtU },
            "ule" => if w { I::I64LeU } else { I::I32LeU },
            "sgt" => if w { I::I64GtS } else { I::I32GtS },
            "sge" => if w { I::I64GeS } else { I::I32GeS },
            "slt" => if w { I::I64LtS } else { I::I32LtS },
            "sle" => if w { I::I64LeS } else { I::I32LeS },
            _ => unsupported!("icmp {pred}"),
        });
        self.define(dest, Ty::I1)
    }

    fn fcmp(&mut self, dest: Option<&str>, args: &str) -> Res<()> {
        let args = skip_flags(args);
        let (pred, rest) = args.split_once(' ').ok_or("fcmp")?;
        let parts = split_top(rest);
        let [a, b] = parts.as_slice() else { unsupported!("fcmp") };
        let (t, a) = a.trim().split_once(' ').ok_or("fcmp")?;
        let Some(ty) = Ty::parse(t).filter(|t| t.float()) else { unsupported!("fcmp on {t}") };
        let f = ty == Ty::F32;
        let (la, lb) = (self.new_local(ty.val()), self.new_local(ty.val()));
        self.value(a, ty)?;
        self.emit(I::LocalSet(la));
        self.value(b, ty)?;
        self.emit(I::LocalSet(lb));
        let cmp = |g: &mut Self, i: I<'static>| {
            g.emit(I::LocalGet(la));
            g.emit(I::LocalGet(lb));
            g.emit(i);
        };
        let (eq, ne, lt, le, gt, ge) = if f {
            (I::F32Eq, I::F32Ne, I::F32Lt, I::F32Le, I::F32Gt, I::F32Ge)
        } else {
            (I::F64Eq, I::F64Ne, I::F64Lt, I::F64Le, I::F64Gt, I::F64Ge)
        };
        // Ordered comparisons are WebAssembly's (false with a NaN); an
        // unordered one is the negation of the opposite ordered one.
        let ord = |g: &mut Self| {
            g.emit(I::LocalGet(la));
            g.emit(I::LocalGet(la));
            g.emit(eq.clone());
            g.emit(I::LocalGet(lb));
            g.emit(I::LocalGet(lb));
            g.emit(eq.clone());
            g.emit(I::I32And);
        };
        match pred {
            "oeq" => cmp(self, eq.clone()),
            "ogt" => cmp(self, gt.clone()),
            "oge" => cmp(self, ge.clone()),
            "olt" => cmp(self, lt.clone()),
            "ole" => cmp(self, le.clone()),
            "une" => cmp(self, ne.clone()),
            "one" => {
                cmp(self, lt.clone());
                cmp(self, gt.clone());
                self.emit(I::I32Or);
            }
            "ord" => ord(self),
            "uno" => {
                ord(self);
                self.emit(I::I32Eqz);
            }
            "ueq" | "ugt" | "uge" | "ult" | "ule" => {
                let opposite = match pred {
                    "ueq" => None,
                    "ugt" => Some(le.clone()),
                    "uge" => Some(lt.clone()),
                    "ult" => Some(ge.clone()),
                    _ => Some(gt.clone()),
                };
                match opposite {
                    Some(i) => cmp(self, i),
                    None => {
                        cmp(self, lt.clone());
                        cmp(self, gt.clone());
                        self.emit(I::I32Or);
                    }
                }
                self.emit(I::I32Eqz);
            }
            "true" => self.emit(I::I32Const(1)),
            "false" => self.emit(I::I32Const(0)),
            _ => unsupported!("fcmp {pred}"),
        }
        self.define(dest, Ty::I1)
    }

    fn cast(&mut self, dest: Option<&str>, op: &str, args: &str) -> Res<()> {
        let (from, to) = args.rsplit_once(" to ").ok_or("cast")?;
        let to = Ty::parse(to).ok_or_else(|| format!("{op} to {to}"))?;
        let ft = self.typed(from)?;
        if to == Ty::I128 || ft == Ty::I128 {
            self.cast128(op, ft, to)?;
            return self.define(dest, to);
        }
        match op {
            "inttoptr" | "ptrtoint" => {}
            "bitcast" => match (ft, to) {
                (a, b) if a == b => {}
                (Ty::Ptr, Ty::I64) | (Ty::I64, Ty::Ptr) => {}
                (Ty::I64, Ty::F64) => self.emit(I::F64ReinterpretI64),
                (Ty::F64, Ty::I64) => self.emit(I::I64ReinterpretF64),
                (Ty::I32, Ty::F32) => self.emit(I::F32ReinterpretI32),
                (Ty::F32, Ty::I32) => self.emit(I::I32ReinterpretF32),
                _ => unsupported!("bitcast {ft:?} to {to:?}"),
            },
            "zext" => {
                if to.wide() && !ft.wide() {
                    self.emit(I::I64ExtendI32U);
                }
            }
            "sext" => {
                self.sign_extend(ft);
                if to.wide() && !ft.wide() {
                    self.emit(I::I64ExtendI32S);
                } else {
                    self.normalize(to);
                }
            }
            "trunc" => {
                if ft.wide() && !to.wide() {
                    self.emit(I::I32WrapI64);
                }
                self.normalize(to);
            }
            "fpext" => self.emit(I::F64PromoteF32),
            "fptrunc" => self.emit(I::F32DemoteF64),
            "sitofp" | "uitofp" => {
                let s = op == "sitofp";
                if s {
                    self.sign_extend(ft);
                }
                self.emit(match (to, ft.wide(), s) {
                    (Ty::F32, false, true) => I::F32ConvertI32S,
                    (Ty::F32, false, false) => I::F32ConvertI32U,
                    (Ty::F32, true, true) => I::F32ConvertI64S,
                    (Ty::F32, true, false) => I::F32ConvertI64U,
                    (_, false, true) => I::F64ConvertI32S,
                    (_, false, false) => I::F64ConvertI32U,
                    (_, true, true) => I::F64ConvertI64S,
                    (_, true, false) => I::F64ConvertI64U,
                });
            }
            // (Out of range is poison in LLVM; saturate, never trap.)
            "fptosi" | "fptoui" => self.float_to_int(ft, to, op == "fptosi"),
            _ => unsupported!("{op}"),
        }
        self.define(dest, to)
    }

    fn float_to_int(&mut self, from: Ty, to: Ty, signed: bool) {
        self.emit(match (to.wide(), from == Ty::F32, signed) {
            (false, true, true) => I::I32TruncSatF32S,
            (false, true, false) => I::I32TruncSatF32U,
            (false, false, true) => I::I32TruncSatF64S,
            (false, false, false) => I::I32TruncSatF64U,
            (true, true, true) => I::I64TruncSatF32S,
            (true, true, false) => I::I64TruncSatF32U,
            (true, false, true) => I::I64TruncSatF64S,
            (true, false, false) => I::I64TruncSatF64U,
        });
        self.normalize(to);
    }

    fn memarg(align: u32) -> MemArg {
        MemArg { offset: 0, align, memory_index: 0 }
    }

    fn load(&mut self, dest: Option<&str>, args: &str) -> Res<()> {
        let atomic = args.starts_with("atomic ");
        let args = args.trim_start_matches("atomic ").trim_start_matches("volatile ");
        let parts = split_top(args);
        let (Some(t), Some(p)) = (parts.first(), parts.get(1)) else { unsupported!("load") };
        let ty = Ty::parse(t).ok_or_else(|| format!("load {t}"))?;
        let p = p.trim();
        if p.starts_with("ptr addrspace(1) @__wasm_env_state") {
            self.emit(I::GlobalGet(self.ctx.state));
            return self.define(dest, ty);
        }
        let p = p.split(" seq_cst").next().unwrap().trim();
        let offset = self.address(p)?;
        let a = if atomic { ty.bits().max(8).trailing_zeros() - 3 } else { 0 };
        let m = MemArg { offset, align: a, memory_index: 0 };
        self.emit(match (ty, atomic) {
            (Ty::I1 | Ty::I8, false) => I::I32Load8U(m),
            (Ty::I16, false) => I::I32Load16U(m),
            (Ty::I32, false) => I::I32Load(m),
            (Ty::I64 | Ty::Ptr, false) => I::I64Load(m),
            (Ty::F32, false) => I::F32Load(m),
            (Ty::F64, false) => I::F64Load(m),
            (Ty::I128, false) => I::V128Load(m),
            (Ty::I1 | Ty::I8, true) => I::I32AtomicLoad8U(m),
            (Ty::I16, true) => I::I32AtomicLoad16U(m),
            (Ty::I32, true) => I::I32AtomicLoad(m),
            (Ty::I64 | Ty::Ptr, true) => I::I64AtomicLoad(m),
            _ => unsupported!("atomic load {ty:?}"),
        });
        self.define(dest, ty)
    }

    fn store(&mut self, args: &str) -> Res<()> {
        let atomic = args.starts_with("atomic ");
        let args = args.trim_start_matches("atomic ").trim_start_matches("volatile ");
        let parts = split_top(args);
        let (Some(v), Some(p)) = (parts.first(), parts.get(1)) else { unsupported!("store") };
        let p = p.split(" seq_cst").next().unwrap().trim();
        if p.contains("addrspace") {
            unsupported!("store to {p}");
        }
        let offset = self.address(p)?;
        let ty = self.typed(v)?;
        let a = if atomic { ty.bits().max(8).trailing_zeros() - 3 } else { 0 };
        let m = MemArg { offset, align: a, memory_index: 0 };
        self.emit(match (ty, atomic) {
            (Ty::I1 | Ty::I8, false) => I::I32Store8(m),
            (Ty::I16, false) => I::I32Store16(m),
            (Ty::I32, false) => I::I32Store(m),
            (Ty::I64 | Ty::Ptr, false) => I::I64Store(m),
            (Ty::F32, false) => I::F32Store(m),
            (Ty::F64, false) => I::F64Store(m),
            (Ty::I128, false) => I::V128Store(m),
            (Ty::I1 | Ty::I8, true) => I::I32AtomicStore8(m),
            (Ty::I16, true) => I::I32AtomicStore16(m),
            (Ty::I32, true) => I::I32AtomicStore(m),
            (Ty::I64 | Ty::Ptr, true) => I::I64AtomicStore(m),
            _ => unsupported!("atomic store {ty:?}"),
        });
        Ok(())
    }

    /// getelementptr [inbounds] [nuw] TY, ptr %base, indices...
    fn gep(&mut self, dest: Option<&str>, args: &str) -> Res<()> {
        let args = args.trim_start_matches("inbounds ").trim_start_matches("nuw ").trim_start_matches("nusw ");
        let parts = split_top(args);
        let (Some(ty), Some(base)) = (parts.first(), parts.get(1)) else { unsupported!("gep") };
        let mut ty = Agg::parse(ty)?;
        self.typed(base)?;
        let mut offset: i64 = 0;
        for (k, idx) in parts[2..].iter().enumerate() {
            let (it, v) = idx.trim().split_once(' ').ok_or("gep index")?;
            let (scale, next) = if k == 0 {
                (ty.size() as i64, ty.clone())
            } else {
                match &ty {
                    Agg::Array(_, e) => (e.size() as i64, (**e).clone()),
                    Agg::Struct(fields) => {
                        let f: usize = v.parse().map_err(|_| "struct index")?;
                        offset += ty.field_offset(f) as i64;
                        ty = fields[f].clone();
                        continue;
                    }
                    _ => unsupported!("gep into a scalar"),
                }
            };
            match v.parse::<i64>() {
                Ok(c) => offset += c * scale,
                Err(_) => {
                    let it = Ty::parse(it).ok_or("gep index type")?;
                    self.value(v, it)?;
                    if !it.wide() {
                        self.sign_extend(it);
                        self.emit(I::I64ExtendI32S);
                    }
                    if scale != 1 {
                        self.emit(I::I64Const(scale));
                        self.emit(I::I64Mul);
                    }
                    self.emit(I::I64Add);
                }
            }
            ty = next;
        }
        if offset != 0 {
            self.emit(I::I64Const(offset));
            self.emit(I::I64Add);
        }
        self.define(dest, Ty::Ptr)
    }

    fn switch<'l>(&mut self, args: &str, rest: &mut std::iter::Peekable<impl Iterator<Item = &'l String>>) -> Res<()> {
        // switch T %v, label %default [ T k, label %l ... ]  (the cases on
        // the following lines)
        let (head, _) = args.split_once('[').ok_or("switch")?;
        let parts = split_top(head);
        let [v, d] = parts.as_slice() else { unsupported!("switch") };
        let (t, v) = v.trim().split_once(' ').ok_or("switch")?;
        let ty = Ty::parse(t).ok_or("switch type")?;
        let default = d.trim().trim_start_matches("label %").to_string();
        let mut cases: Vec<(i64, String)> = Vec::new();
        for line in rest.by_ref() {
            let l = line.trim();
            if l == "]" {
                break;
            }
            let (k, target) = l.split_once(", label %").ok_or("switch case")?;
            let k: i64 = k.trim().split_whitespace().last().unwrap_or("").parse().map_err(|_| "switch key")?;
            cases.push((k, target.to_string()));
        }
        let key = self.new_local(ty.val());
        self.value(v, ty)?;
        self.emit(I::LocalSet(key));
        // Dense (the region's entries, 4 bytes apart within a page): a
        // br_table on (key - min) / 4, through a block per distinct target.
        let (min, max) = (cases.iter().map(|c| c.0).min().unwrap_or(0), cases.iter().map(|c| c.0).max().unwrap_or(0));
        let dense = ty.wide() && !cases.is_empty() && max - min < 4 * 2048 && cases.iter().all(|c| (c.0 - min) % 4 == 0);
        if dense {
            let mut targets: Vec<String> = cases.iter().map(|c| c.1.clone()).collect();
            targets.sort();
            targets.dedup();
            targets.push(default.clone());
            let slots = ((max - min) / 4 + 1) as usize;
            let mut table = vec![targets.len() as u32 - 1; slots];
            for (k, t) in &cases {
                table[((k - min) / 4) as usize] = targets.iter().position(|x| x == t).unwrap() as u32;
            }
            // block_d { block_{m-1} ... { block_0 { idx; br_table } goto t_0 } ... } goto default
            let m = targets.len();
            for _ in 0..m {
                self.emit(I::Block(BlockType::Empty));
            }
            self.extra += m as u32;
            // Out of range or misaligned: the default.
            self.emit(I::LocalGet(key));
            self.emit(I::I64Const(min));
            self.emit(I::I64Sub);
            let idx = self.new_local(ValType::I64);
            self.emit(I::LocalTee(idx));
            self.emit(I::I64Const(3));
            self.emit(I::I64And);
            self.emit(I::I64Eqz);
            self.emit(I::If(BlockType::Empty));
            self.extra += 1;
            self.emit(I::LocalGet(idx));
            self.emit(I::I64Const(2));
            self.emit(I::I64ShrU);
            self.emit(I::LocalSet(idx));
            self.emit(I::LocalGet(idx));
            self.emit(I::I64Const(slots as i64));
            self.emit(I::I64LtU);
            self.emit(I::If(BlockType::Empty));
            self.extra += 1;
            self.emit(I::LocalGet(idx));
            self.emit(I::I32WrapI64);
            // (Two ifs deep: the blocks' depths are two more.)
            let labels: Vec<u32> = table.iter().map(|&t| t + 2).collect();
            self.emit(I::BrTable(labels.into(), m as u32 - 1 + 2));
            self.emit(I::End);
            self.emit(I::End);
            self.extra -= 2;
            self.emit(I::Br(m as u32 - 1));
            for t in &targets {
                self.emit(I::End);
                self.extra -= 1;
                self.goto(t, false)?;
            }
            return Ok(());
        }
        for (k, target) in &cases {
            self.emit(I::LocalGet(key));
            self.emit(if ty.wide() { I::I64Const(*k) } else { I::I32Const(*k as i32) });
            self.emit(if ty.wide() { I::I64Eq } else { I::I32Eq });
            self.emit(I::If(BlockType::Empty));
            self.extra += 1;
            self.goto(target, false)?;
            self.emit(I::End);
            self.extra -= 1;
        }
        self.goto(&default, false)
    }

    fn call(&mut self, dest: Option<&str>, args: &str) -> Res<()> {
        // call RET @name(T a, T b...)
        let (ret, rest) = args.split_once(" @").ok_or_else(|| format!("call {args}"))?;
        let (name, a) = rest.split_once('(').ok_or("call")?;
        let a = a.strip_suffix(')').ok_or("call args")?;
        let ret = ret.split_whitespace().last().unwrap_or("void");
        let ret_ty = if ret == "void" { None } else { Some(Ty::parse(ret).ok_or_else(|| format!("call returning {ret}"))?) };
        let params: Vec<&str> = split_top(a).into_iter().filter(|s| !s.trim().is_empty()).collect();
        if let Some(intrinsic) = name.strip_prefix("llvm.") {
            let ty = self.intrinsic(intrinsic, &params, ret_ty)?;
            return match ty {
                Some(t) => self.define(dest, t),
                None => Ok(()),
            };
        }
        let Some(&f) = self.ctx.func_index.get(name) else { unsupported!("call {name}") };
        if !self.ctx.import_sig.contains_key(name) {
            unsupported!("call to region {name}");
        }
        for p in &params {
            self.typed(p)?;
        }
        self.emit(I::Call(f));
        match ret_ty {
            Some(t) => self.define(dest, t),
            None => Ok(()),
        }
    }

    fn intrinsic(&mut self, name: &str, params: &[&str], ret: Option<Ty>) -> Res<Option<Ty>> {
        let base = name.split('.').next().unwrap_or("");
        let ty = ret.ok_or("intrinsic without a result")?;
        if ty == Ty::I128 {
            unsupported!("llvm.{name} on i128");
        }
        let w = ty.wide();
        let f32 = ty == Ty::F32;
        match base {
            "umax" | "umin" | "smax" | "smin" => {
                let (a, b) = (self.new_local(ty.val()), self.new_local(ty.val()));
                self.typed(params[0])?;
                self.emit(I::LocalSet(a));
                self.typed(params[1])?;
                self.emit(I::LocalSet(b));
                self.emit(I::LocalGet(a));
                self.emit(I::LocalGet(b));
                self.emit(I::LocalGet(a));
                if base.starts_with('s') {
                    self.sign_extend(ty);
                }
                self.emit(I::LocalGet(b));
                if base.starts_with('s') {
                    self.sign_extend(ty);
                }
                self.emit(match (base, w) {
                    ("umax", false) => I::I32GtU,
                    ("umax", true) => I::I64GtU,
                    ("umin", false) => I::I32LtU,
                    ("umin", true) => I::I64LtU,
                    ("smax", false) => I::I32GtS,
                    ("smax", true) => I::I64GtS,
                    ("smin", false) => I::I32LtS,
                    _ => I::I64LtS,
                });
                self.emit(I::Select);
            }
            "ctpop" => {
                self.typed(params[0])?;
                self.emit(if w { I::I64Popcnt } else { I::I32Popcnt });
            }
            "ctlz" => {
                self.typed(params[0])?;
                self.emit(if w { I::I64Clz } else { I::I32Clz });
                if ty.narrow() {
                    self.emit(I::I32Const(32 - ty.bits() as i32));
                    self.emit(I::I32Sub);
                }
            }
            "cttz" => {
                self.typed(params[0])?;
                if ty.narrow() {
                    self.emit(I::I32Const(1 << ty.bits()));
                    self.emit(I::I32Or);
                }
                self.emit(if w { I::I64Ctz } else { I::I32Ctz });
            }
            "bswap" => {
                self.typed(params[0])?;
                self.bswap(ty)?;
            }
            "bitreverse" => {
                self.typed(params[0])?;
                self.bitreverse(ty)?;
            }
            "fshr" | "fshl" => {
                // fshr(a, b, s): the low half of (a:b) >> s; fshl: the high
                // half of (a:b) << s (s modulo the width).
                if ty.narrow() {
                    unsupported!("{name}");
                }
                let bits = ty.bits() as i64;
                let (a, b, s) = (self.new_local(ty.val()), self.new_local(ty.val()), self.new_local(ty.val()));
                for (l, p) in [(a, params[0]), (b, params[1]), (s, params[2])] {
                    self.typed(p)?;
                    self.emit(I::LocalSet(l));
                }
                let c = |g: &mut Self, v: i64| g.emit(if w { I::I64Const(v) } else { I::I32Const(v as i32) });
                self.emit(I::LocalGet(s));
                c(self, bits - 1);
                self.emit(if w { I::I64And } else { I::I32And });
                self.emit(I::LocalSet(s));
                let (hi, lo, shl, shr) = if base == "fshr" {
                    (a, b, if w { I::I64Shl } else { I::I32Shl }, if w { I::I64ShrU } else { I::I32ShrU })
                } else {
                    (b, a, if w { I::I64ShrU } else { I::I32ShrU }, if w { I::I64Shl } else { I::I32Shl })
                };
                // (lo >> s) | (hi << (bits - s)), or lo when s == 0.
                self.emit(I::LocalGet(lo));
                self.emit(I::LocalGet(s));
                self.emit(shr);
                self.emit(I::LocalGet(hi));
                c(self, bits);
                self.emit(I::LocalGet(s));
                self.emit(if w { I::I64Sub } else { I::I32Sub });
                self.emit(shl);
                self.emit(if w { I::I64Or } else { I::I32Or });
                self.emit(I::LocalGet(lo));
                self.emit(I::LocalGet(s));
                self.emit(if w { I::I64Eqz } else { I::I32Eqz });
                self.emit(I::Select);
            }
            "fabs" | "sqrt" | "floor" | "ceil" | "trunc" | "rint" | "nearbyint" | "roundeven" => {
                self.typed(params[0])?;
                self.emit(match (base, f32) {
                    ("fabs", true) => I::F32Abs,
                    ("fabs", false) => I::F64Abs,
                    ("sqrt", true) => I::F32Sqrt,
                    ("sqrt", false) => I::F64Sqrt,
                    ("floor", true) => I::F32Floor,
                    ("floor", false) => I::F64Floor,
                    ("ceil", true) => I::F32Ceil,
                    ("ceil", false) => I::F64Ceil,
                    ("trunc", true) => I::F32Trunc,
                    ("trunc", false) => I::F64Trunc,
                    (_, true) => I::F32Nearest,
                    (_, false) => I::F64Nearest,
                });
            }
            "minimum" | "maximum" | "copysign" => {
                self.typed(params[0])?;
                self.typed(params[1])?;
                self.emit(match (base, f32) {
                    ("minimum", true) => I::F32Min,
                    ("minimum", false) => I::F64Min,
                    ("maximum", true) => I::F32Max,
                    ("maximum", false) => I::F64Max,
                    ("copysign", true) => I::F32Copysign,
                    _ => I::F64Copysign,
                });
            }
            "arithmetic" => {
                self.typed(params[0])?; // arithmetic.fence: the value
            }
            "fptosi" | "fptoui" => {
                let from = self.typed(params[0])?;
                self.float_to_int(from, ty, base == "fptosi");
            }
            _ => unsupported!("llvm.{name}"),
        }
        Ok(Some(ty))
    }

    /// Reverses the bytes of the value on the stack.
    fn bswap(&mut self, ty: Ty) -> Res<()> {
        match ty {
            Ty::I16 => {
                let x = self.new_local(ValType::I32);
                self.emit(I::LocalTee(x));
                self.emit(I::I32Const(8));
                self.emit(I::I32ShrU);
                self.emit(I::LocalGet(x));
                self.emit(I::I32Const(8));
                self.emit(I::I32Shl);
                self.emit(I::I32Or);
                self.normalize(Ty::I16);
            }
            Ty::I32 => self.bswap32()?,
            Ty::I64 => {
                // bswap64(x) = bswap32(lo) << 32 | bswap32(hi)
                let x = self.new_local(ValType::I64);
                self.emit(I::LocalTee(x));
                self.emit(I::I32WrapI64);
                self.bswap32()?;
                self.emit(I::I64ExtendI32U);
                self.emit(I::I64Const(32));
                self.emit(I::I64Shl);
                self.emit(I::LocalGet(x));
                self.emit(I::I64Const(32));
                self.emit(I::I64ShrU);
                self.emit(I::I32WrapI64);
                self.bswap32()?;
                self.emit(I::I64ExtendI32U);
                self.emit(I::I64Or);
            }
            _ => unsupported!("bswap {ty:?}"),
        }
        Ok(())
    }

    fn bswap32(&mut self) -> Res<()> {
        let x = self.new_local(ValType::I32);
        self.emit(I::LocalSet(x));
        // (x << 24) | ((x & 0xff00) << 8) | ((x >> 8) & 0xff00) | (x >> 24)
        self.emit(I::LocalGet(x));
        self.emit(I::I32Const(24));
        self.emit(I::I32Shl);
        self.emit(I::LocalGet(x));
        self.emit(I::I32Const(0xff00));
        self.emit(I::I32And);
        self.emit(I::I32Const(8));
        self.emit(I::I32Shl);
        self.emit(I::I32Or);
        self.emit(I::LocalGet(x));
        self.emit(I::I32Const(8));
        self.emit(I::I32ShrU);
        self.emit(I::I32Const(0xff00));
        self.emit(I::I32And);
        self.emit(I::I32Or);
        self.emit(I::LocalGet(x));
        self.emit(I::I32Const(24));
        self.emit(I::I32ShrU);
        self.emit(I::I32Or);
        Ok(())
    }

    /// Reverses the bits of the value on the stack: the bytes, then the
    /// bits within each byte (swapping nibbles, pairs, then bits).
    fn bitreverse(&mut self, ty: Ty) -> Res<()> {
        if ty.narrow() && ty != Ty::I8 && ty != Ty::I16 {
            unsupported!("bitreverse {ty:?}");
        }
        if ty != Ty::I8 {
            self.bswap(ty)?;
        }
        let w = ty.wide();
        let x = self.new_local(ty.val());
        for (shift, mask) in [(4, 0x0f0f_0f0f_0f0f_0f0fu64), (2, 0x3333_3333_3333_3333), (1, 0x5555_5555_5555_5555)] {
            // x = ((x >> s) & m) | ((x & m) << s)
            self.emit(I::LocalSet(x));
            let c = |v: u64| if w { I::I64Const(v as i64) } else { I::I32Const(v as u32 as i32) };
            self.emit(I::LocalGet(x));
            self.emit(c(shift));
            self.emit(if w { I::I64ShrU } else { I::I32ShrU });
            self.emit(c(mask));
            self.emit(if w { I::I64And } else { I::I32And });
            self.emit(I::LocalGet(x));
            self.emit(c(mask));
            self.emit(if w { I::I64And } else { I::I32And });
            self.emit(c(shift));
            self.emit(if w { I::I64Shl } else { I::I32Shl });
            self.emit(if w { I::I64Or } else { I::I32Or });
        }
        self.normalize(ty);
        Ok(())
    }

    /// A cast to or from i128 (a v128), its operand on the stack: zext and
    /// sext build the vector from a 64-bit value, trunc takes its low lane.
    fn cast128(&mut self, op: &str, from: Ty, to: Ty) -> Res<()> {
        match (op, to) {
            ("zext" | "sext", Ty::I128) => {
                let signed = op == "sext";
                if signed {
                    self.sign_extend(from);
                }
                if !from.wide() {
                    self.emit(if signed { I::I64ExtendI32S } else { I::I64ExtendI32U });
                }
                let x = self.new_local(ValType::I64);
                self.emit(I::LocalSet(x));
                if signed {
                    self.emit(I::LocalGet(x));
                    self.emit(I::I64Const(63));
                    self.emit(I::I64ShrS);
                    self.emit(I::I64x2Splat);
                } else {
                    self.emit(I::V128Const(0));
                }
                self.emit(I::LocalGet(x));
                self.emit(I::I64x2ReplaceLane(0));
            }
            ("trunc", _) if from == Ty::I128 => {
                self.emit(I::I64x2ExtractLane(0));
                if !to.wide() {
                    self.emit(I::I32WrapI64);
                }
                self.normalize(to);
            }
            _ => unsupported!("{op} {from:?} to {to:?}"),
        }
        Ok(())
    }

    /// Bitwise operations on i128 (v128), and shifts by a constant, by way
    /// of its two 64-bit halves.
    fn binop128(&mut self, op: &str, a: &str, b: &str) -> Res<()> {
        let bitwise = match op {
            "and" => Some(I::V128And),
            "or" => Some(I::V128Or),
            "xor" => Some(I::V128Xor),
            _ => None,
        };
        if let Some(i) = bitwise {
            self.value(a, Ty::I128)?;
            self.value(b, Ty::I128)?;
            self.emit(i);
            return Ok(());
        }
        let Ok(k) = b.trim().parse::<u32>() else { unsupported!("{op} i128 by {b}") };
        if k >= 128 || !matches!(op, "shl" | "lshr" | "ashr") {
            unsupported!("{op} i128 {k}");
        }
        let (lo, hi) = (self.new_local(ValType::I64), self.new_local(ValType::I64));
        self.value(a, Ty::I128)?;
        let v = self.new_local(ValType::V128);
        self.emit(I::LocalTee(v));
        self.emit(I::I64x2ExtractLane(0));
        self.emit(I::LocalSet(lo));
        self.emit(I::LocalGet(v));
        self.emit(I::I64x2ExtractLane(1));
        self.emit(I::LocalSet(hi));
        // The new halves, as i64 expressions of lo and hi.
        let k = k as i64;
        let get = |g: &mut Self, l| g.emit(I::LocalGet(l));
        let konst = |g: &mut Self, v: i64| g.emit(I::I64Const(v));
        let (new_lo, new_hi): (Box<dyn Fn(&mut Self)>, Box<dyn Fn(&mut Self)>) = match op {
            "shl" if k == 0 => (Box::new(move |g| get(g, lo)), Box::new(move |g| get(g, hi))),
            "shl" if k < 64 => (
                Box::new(move |g| { get(g, lo); konst(g, k); g.emit(I::I64Shl); }),
                Box::new(move |g| { get(g, hi); konst(g, k); g.emit(I::I64Shl); get(g, lo); konst(g, 64 - k); g.emit(I::I64ShrU); g.emit(I::I64Or); }),
            ),
            "shl" => (Box::new(move |g| konst(g, 0)), Box::new(move |g| { get(g, lo); konst(g, k - 64); g.emit(I::I64Shl); })),
            _ if k == 0 => (Box::new(move |g| get(g, lo)), Box::new(move |g| get(g, hi))),
            _ if k < 64 => {
                let sh = if op == "ashr" { I::I64ShrS } else { I::I64ShrU };
                (
                    Box::new(move |g| { get(g, lo); konst(g, k); g.emit(I::I64ShrU); get(g, hi); konst(g, 64 - k); g.emit(I::I64Shl); g.emit(I::I64Or); }),
                    Box::new(move |g| { get(g, hi); konst(g, k); g.emit(sh.clone()); }),
                )
            }
            _ => {
                let arith = op == "ashr";
                (
                    Box::new(move |g| { get(g, hi); konst(g, k - 64); g.emit(if arith { I::I64ShrS } else { I::I64ShrU }); }),
                    Box::new(move |g| { if arith { get(g, hi); konst(g, 63); g.emit(I::I64ShrS); } else { konst(g, 0); } }),
                )
            }
        };
        new_hi(self);
        self.emit(I::I64x2Splat);
        new_lo(self);
        self.emit(I::I64x2ReplaceLane(0));
        Ok(())
    }

    fn cmpxchg(&mut self, dest: Option<&str>, args: &str) -> Res<()> {
        // cmpxchg ptr %p, T %expected, T %new seq_cst seq_cst
        let parts = split_top(args);
        let [p, e, n] = parts.as_slice() else { unsupported!("cmpxchg") };
        let n = n.trim().split(" seq_cst").next().unwrap();
        self.typed(p)?;
        let ty = self.typed(e)?;
        let expected = self.new_local(ty.val());
        self.emit(I::LocalTee(expected));
        self.emit(I::Drop);
        self.emit(I::LocalGet(expected));
        self.typed(n)?;
        let m = Self::memarg(ty.bits().max(8).trailing_zeros() - 3);
        self.emit(match ty {
            Ty::I8 => I::I32AtomicRmw8CmpxchgU(m),
            Ty::I16 => I::I32AtomicRmw16CmpxchgU(m),
            Ty::I32 => I::I32AtomicRmwCmpxchg(m),
            Ty::I64 | Ty::Ptr => I::I64AtomicRmwCmpxchg(m),
            _ => unsupported!("cmpxchg {ty:?}"),
        });
        let old = self.new_local(ty.val());
        self.emit(I::LocalTee(old));
        self.emit(I::LocalGet(expected));
        self.emit(if ty.wide() { I::I64Eq } else { I::I32Eq });
        let ok = self.new_local(ValType::I32);
        self.emit(I::LocalSet(ok));
        let Some(d) = dest else { unsupported!("cmpxchg without a result") };
        self.locals.insert(format!("{d}.old"), (old, ty));
        self.pairs.insert(d.to_string(), (old, ok));
        Ok(())
    }

    fn atomicrmw(&mut self, dest: Option<&str>, args: &str) -> Res<()> {
        // atomicrmw op ptr %p, T %v seq_cst
        let (op, rest) = args.split_once(' ').ok_or("atomicrmw")?;
        let parts = split_top(rest);
        let [p, v] = parts.as_slice() else { unsupported!("atomicrmw") };
        let v = v.trim().split(" seq_cst").next().unwrap();
        self.typed(p)?;
        let ty = self.typed(v)?;
        let m = Self::memarg(ty.bits().max(8).trailing_zeros() - 3);
        let i = match (op, ty) {
            ("add", Ty::I64 | Ty::Ptr) => I::I64AtomicRmwAdd(m),
            ("add", Ty::I32) => I::I32AtomicRmwAdd(m),
            ("add", Ty::I16) => I::I32AtomicRmw16AddU(m),
            ("add", Ty::I8) => I::I32AtomicRmw8AddU(m),
            ("sub", Ty::I64 | Ty::Ptr) => I::I64AtomicRmwSub(m),
            ("sub", Ty::I32) => I::I32AtomicRmwSub(m),
            ("and", Ty::I64 | Ty::Ptr) => I::I64AtomicRmwAnd(m),
            ("and", Ty::I32) => I::I32AtomicRmwAnd(m),
            ("and", Ty::I16) => I::I32AtomicRmw16AndU(m),
            ("and", Ty::I8) => I::I32AtomicRmw8AndU(m),
            ("or", Ty::I64 | Ty::Ptr) => I::I64AtomicRmwOr(m),
            ("or", Ty::I32) => I::I32AtomicRmwOr(m),
            ("or", Ty::I16) => I::I32AtomicRmw16OrU(m),
            ("or", Ty::I8) => I::I32AtomicRmw8OrU(m),
            ("xor", Ty::I64 | Ty::Ptr) => I::I64AtomicRmwXor(m),
            ("xor", Ty::I32) => I::I32AtomicRmwXor(m),
            ("xor", Ty::I16) => I::I32AtomicRmw16XorU(m),
            ("xor", Ty::I8) => I::I32AtomicRmw8XorU(m),
            ("xchg", Ty::I64 | Ty::Ptr) => I::I64AtomicRmwXchg(m),
            ("xchg", Ty::I32) => I::I32AtomicRmwXchg(m),
            ("xchg", Ty::I16) => I::I32AtomicRmw16XchgU(m),
            ("xchg", Ty::I8) => I::I32AtomicRmw8XchgU(m),
            _ => unsupported!("atomicrmw {op} {ty:?}"),
        };
        self.emit(i);
        self.define(dest, ty)
    }
}

/// An aggregate type in a getelementptr: its size and field offsets.
#[derive(Clone, Debug)]
enum Agg {
    Scalar(u64),
    Array(u64, Box<Agg>),
    Struct(Vec<Agg>),
}

impl Agg {
    fn parse(s: &str) -> Res<Agg> {
        let s = s.trim();
        if let Some(inner) = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            let (n, e) = inner.split_once(" x ").ok_or("array type")?;
            return Ok(Agg::Array(n.trim().parse().map_err(|_| "array length")?, Box::new(Agg::parse(e)?)));
        }
        if let Some(inner) = s.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
            return Ok(Agg::Struct(split_top(inner).into_iter().map(Agg::parse).collect::<Res<_>>()?));
        }
        Ok(Agg::Scalar(match s {
            "i1" | "i8" => 1,
            "i16" | "half" => 2,
            "i32" | "float" => 4,
            "i64" | "ptr" | "double" => 8,
            "i128" => 16,
            _ => unsupported!("type {s}"),
        }))
    }
    fn align(&self) -> u64 {
        match self {
            Agg::Scalar(s) => *s,
            Agg::Array(_, e) => e.align(),
            Agg::Struct(f) => f.iter().map(Agg::align).max().unwrap_or(1),
        }
    }
    fn size(&self) -> u64 {
        match self {
            Agg::Scalar(s) => *s,
            Agg::Array(n, e) => n * e.size(),
            Agg::Struct(f) => {
                let end = self.field_offset(f.len());
                end.next_multiple_of(self.align())
            }
        }
    }
    fn field_offset(&self, k: usize) -> u64 {
        let Agg::Struct(fields) = self else { return 0 };
        let mut at: u64 = 0;
        for (i, f) in fields.iter().enumerate() {
            at = at.next_multiple_of(f.align());
            if i == k {
                return at;
            }
            at += f.size();
        }
        at
    }
}

/// A getelementptr (its operands) whose indices are all constants: its base
/// value and byte offset.
fn gep_constant(args: &str) -> Option<(String, i64)> {
    let args = args.trim_start_matches("inbounds ").trim_start_matches("nuw ").trim_start_matches("nusw ");
    let parts = split_top(args);
    let mut ty = Agg::parse(parts.first()?).ok()?;
    let base = parts.get(1)?.trim().strip_prefix("ptr ")?.trim();
    if !base.starts_with('%') {
        return None;
    }
    let mut offset = 0i64;
    for (k, idx) in parts[2..].iter().enumerate() {
        let (_, v) = idx.trim().split_once(' ')?;
        let c: i64 = v.parse().ok()?;
        if k == 0 {
            offset += c * ty.size() as i64;
            continue;
        }
        match ty.clone() {
            Agg::Array(_, e) => {
                offset += c * e.size() as i64;
                ty = *e;
            }
            Agg::Struct(fields) => {
                offset += ty.field_offset(c as usize) as i64;
                ty = fields.get(c as usize)?.clone();
            }
            Agg::Scalar(_) => return None,
        }
    }
    Some((base.to_string(), offset))
}

/// Splits on commas outside brackets, braces, parentheses and angles.
fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '[' | '{' | '(' | '<' => depth += 1,
            ']' | '}' | ')' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Drops metadata (`, !tbaa !902`) and alignment (`, align 8`).
fn strip_metadata(line: &str) -> String {
    let parts = split_top(line);
    let kept: Vec<&str> = parts.into_iter().filter(|p| !p.trim_start().starts_with('!') && !p.trim_start().starts_with("align ")).collect();
    kept.join(",")
}

/// Skips instruction flags (nuw, nsw, exact, fast-math flags).
fn skip_flags(s: &str) -> &str {
    let mut s = s.trim_start();
    loop {
        let Some((w, rest)) = s.split_once(' ') else { return s };
        if matches!(w, "nuw" | "nsw" | "exact" | "disjoint" | "fast" | "nnan" | "ninf" | "nsz" | "arcp" | "contract" | "afn" | "reassoc") {
            s = rest.trim_start();
        } else {
            return s;
        }
    }
}

/// An LLVM string constant's bytes (`\XX` escapes).
fn unescape(s: &str) -> Res<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let hex = s.get(i + 1..i + 3).ok_or("escape")?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| "escape")?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A region function as the lifter writes one: state pointers, the
    /// entry switch, register loads and stores, the dispatch cache probe,
    /// tail calls, a phi, a narrow add, an i128 register write and a fatal
    /// message.
    const CHUNK: &str = r#"target triple = "wasm64-unknown-unknown"
@__wasm_env_state = external addrspace(1) global i64
declare void @dispatch(i64)
declare void @__wasm_env_fatal(ptr, i64)
declare void @__wasm_env_bad_target(i64)
declare i32 @__wasm_env_fp_flags(double, double, double, double, i32) memory(none)
declare i8 @llvm.umax.i8(i8, i8)
@S_1000_0 = private constant [4 x i8] c"bad\00"
define void @R_1000(i64 %pc) {
entry:
  %st = load i64, ptr addrspace(1) @__wasm_env_state, align 8, !invariant.load !906
  %R = inttoptr i64 %st to ptr
  %V = getelementptr inbounds nuw i8, ptr %R, i64 288
  %DC = getelementptr inbounds nuw i8, ptr %R, i64 800
  switch i64 %pc, label %bad [
    i64 4096, label %b_1000
    i64 4104, label %b_1008
  ]
b_1000:
  %t0 = getelementptr inbounds nuw [32 x i64], ptr %R, i64 0, i64 1
  %t1 = load i64, ptr %t0, !tbaa !902
  %t2 = add nuw i64 %t1, 16
  %t3 = trunc i64 %t2 to i8
  %t4 = call i8 @llvm.umax.i8(i8 %t3, i8 200)
  %t5 = zext i8 %t4 to i128
  %t6 = getelementptr inbounds nuw [32 x i128], ptr %V, i64 0, i64 3
  store i128 %t5, ptr %t6, align 16, !tbaa !902
  %t7 = icmp eq i64 %t1, 0
  br i1 %t7, label %L8, label %L9
L8:
  br label %L9
L9:
  %t10 = phi i64 [ 1, %b_1000 ], [ 2, %L8 ]
  %t11 = lshr i64 %t10, 12
  %t12 = and i64 %t11, 4095
  %t13 = getelementptr inbounds nuw [4096 x { i64, ptr }], ptr %DC, i64 0, i64 %t12, i32 1
  %t14 = load ptr, ptr %t13
  musttail call void %t14(i64 %t10)
  ret void
b_1008:
  call void @__wasm_env_fatal(ptr @S_1000_0, i64 4104)
  unreachable
bad:
  call void @__wasm_env_bad_target(i64 %pc)
  unreachable
}
"#;

    #[test]
    fn compiles_a_region_to_a_valid_side_module() {
        let wasm = compile(&[CHUNK.to_string()]).expect("supported");
        let mut config = wasmtime::Config::new();
        config.wasm_threads(true).wasm_memory64(true).wasm_tail_call(true).shared_memory(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let module = wasmtime::Module::new(&engine, &wasm).expect("valid");
        let exports: Vec<&str> = module.exports().map(|e| e.name()).collect();
        for name in ["R_1000", "dispatch_addrs", "dispatch_fns", "dispatch_count", "__wasm_apply_data_relocs"] {
            assert!(exports.contains(&name), "{name} exported");
        }
        assert!(module.imports().any(|i| i.module() == "GOT.func" && i.name() == "R_1000"));
    }

    #[test]
    fn declines_what_it_does_not_compile() {
        let vector = CHUNK.replace("%t5 = zext i8 %t4 to i128", "%t5 = bitcast <2 x i64> zeroinitializer to i128");
        assert!(compile(&[vector]).is_err());
    }

    #[test]
    fn lays_out_aggregates_like_llvm() {
        let entry = Agg::parse("[4096 x { i64, ptr }]").unwrap();
        assert_eq!(entry.size(), 4096 * 16);
        assert_eq!(Agg::parse("{ i8, i64 }").unwrap().field_offset(1), 8);
        assert_eq!(Agg::parse("[32 x i128]").unwrap().size(), 512);
    }
}
