//! OpenCL C to LLVM IR, by clang (the toolchain translation uses): the
//! spir target (32-bit pointers and size_t, which suit WGSL's u32), and
//! everything inlined into the kernels.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::ir::{Module, Res, parse};

/// What compiles OpenCL C: clang, and OpenCL's builtins as LLVM bitcode
/// (libclc, packages/libclc), linked into programs for accurate math. Without
/// them, the math builtins are the GPU's own (WGSL's), looser than OpenCL
/// wants.
pub struct Compiler {
    pub clang: std::path::PathBuf,
    pub builtins: Option<std::path::PathBuf>,
}

impl Compiler {
    /// clang's arguments linking the builtins in (only what's used).
    fn builtin_args(&self) -> Vec<String> {
        self.builtins.iter().flat_map(|b| ["-Xclang".to_string(), "-mlink-builtin-bitcode".into(), "-Xclang".into(), b.display().to_string()]).collect()
    }
}

/// Where the program that builds is: its environment's root on the host,
/// and its working directory (on the host). clang runs on the host, so the
/// program's paths (`-I /opt/include`, `#include "/abs/path.h"`, relative
/// ones) are mapped to these.
pub struct Guest {
    pub root: std::path::PathBuf,
    pub cwd: std::path::PathBuf,
}

impl Guest {
    /// clang's arguments for it: a filesystem overlay putting the root at
    /// `/` (falling through to the host's, where clang's own headers are),
    /// and the working directory.
    fn args(&self, dir: &Scratch) -> Result<Vec<String>, String> {
        let overlay = dir.0.join("root.yaml");
        // (Each of the root's directories: clang can't take / itself remapped.)
        let quote = |s: String| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut roots = Vec::new();
        for e in std::fs::read_dir(&self.root).map_err(|e| e.to_string())?.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                let name = quote(format!("/{}", e.file_name().to_string_lossy()));
                let to = quote(e.path().display().to_string());
                roots.push(format!("{{\"name\": \"{name}\", \"type\": \"directory-remap\", \"external-contents\": \"{to}\"}}"));
            }
        }
        let yaml = format!("{{\"version\": 0, \"redirecting-with\": \"fallthrough\", \"roots\": [{}]}}\n", roots.join(", "));
        std::fs::write(&overlay, yaml).map_err(|e| e.to_string())?;
        Ok(vec!["-ivfsoverlay".into(), overlay.display().to_string(), "-working-directory".into(), self.cwd.display().to_string()])
    }
}

/// A program's source compiled: its module, or clang's messages (the build
/// log) on failure.
pub fn build(cc: &Compiler, source: &str, options: &str, guest: Option<&Guest>) -> Result<Module, String> {
    let ir = clang_ir(cc, source, options, guest)?;
    parse(&ir)
}

/// OpenCL options clang takes as they are; others (-cl-opt-disable and the
/// like, which only tune code generation) are dropped.
fn passed(options: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut words = options.split_whitespace().peekable();
    while let Some(w) = words.next() {
        if w == "-D" || w == "-I" {
            if let Some(v) = words.next() {
                out.push(w.to_string());
                out.push(v.to_string());
            }
        } else if w.starts_with("-D") || w.starts_with("-I") || w == "-w" || w == "-Werror" || w.starts_with("-cl-std=") {
            out.push(w.to_string());
        } else if matches!(w, "-cl-fast-relaxed-math" | "-cl-mad-enable" | "-cl-finite-math-only" | "-cl-unsafe-math-optimizations" | "-cl-no-signed-zeros" | "-cl-denorms-are-zero" | "-cl-single-precision-constant") {
            out.push(w.to_string());
        }
    }
    out
}

pub fn clang_ir(cc: &Compiler, source: &str, options: &str, guest: Option<&Guest>) -> Res<String> {
    let clang = &cc.clang;
    let dir = Scratch::new()?;
    // (No __IMAGE_SUPPORT__: the device has no images. __OPENCL_VERSION__ is
    // the device's, which clang leaves to the driver.)
    let mut args: Vec<String> = ["-x", "cl", "-cl-std=CL1.2", "-target", "spir", "-O2", "-emit-llvm", "-S", "-Xclang", "-finclude-default-header", "-cl-kernel-arg-info", "-U__IMAGE_SUPPORT__", "-D__OPENCL_VERSION__=120"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // Everything inlined: the WGSL is one function per kernel.
    args.extend(["-mllvm", "-inline-threshold=100000"].map(String::from));
    args.extend(passed(options));
    args.extend(cc.builtin_args());
    if let Some(g) = guest {
        args.extend(g.args(&dir)?);
    }
    args.extend(["-o", "-", "-"].map(String::from));
    let mut child = Command::new(clang)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't run {}: {e}", clang.display()))?;
    child.stdin.take().unwrap().write_all(source.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).replace("<stdin>", "program"));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) fn test_clang() -> Compiler {
    let clang = std::env::var_os("WASM_CC").map_or_else(|| "clang".into(), Into::into);
    // (The builtins too, if packages/libclc/build.sh has built them.)
    let builtins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../build/libclc/clspv--.bc");
    Compiler { clang, builtins: builtins.exists().then_some(builtins) }
}

/// Separate compilation (clCompileProgram): a program's source, with the
/// headers it includes by name, to an object: LLVM bitcode, unoptimized
/// (and not marked unoptimizable) so that linking optimizes the whole.
pub fn compile(cc: &Compiler, source: &str, options: &str, headers: &[(String, Vec<u8>)], guest: Option<&Guest>) -> Result<Vec<u8>, String> {
    // (The builtins come in when it's linked.)
    let clang = &cc.clang;
    let dir = Scratch::new()?;
    for (name, text) in headers {
        let path = dir.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
    }
    let mut args: Vec<String> = ["-x", "cl", "-cl-std=CL1.2", "-target", "spir", "-O2", "-Xclang", "-disable-llvm-passes", "-c", "-emit-llvm"]
        .iter()
        .chain(&["-Xclang", "-finclude-default-header", "-cl-kernel-arg-info", "-U__IMAGE_SUPPORT__", "-D__OPENCL_VERSION__=120"])
        .map(|s| s.to_string())
        .collect();
    args.extend(["-I".to_string(), dir.0.display().to_string()]);
    args.extend(passed(options));
    if let Some(g) = guest {
        args.extend(g.args(&dir)?);
    }
    args.extend(["-o", "-", "-"].map(String::from));
    run(clang, &args, source.as_bytes())
}

pub enum Linked {
    /// The module, and its bitcode (its binary).
    Executable(Module, Vec<u8>),
    /// A library (`-create-library`): an object, as bitcode.
    Library(Vec<u8>),
}

/// Linking (clLinkProgram): objects into one module, optimized and
/// inlined as a built program is, or into a library.
pub fn link(cc: &Compiler, objects: &[Vec<u8>], library: bool) -> Result<Linked, String> {
    let clang = &cc.clang;
    let dir = Scratch::new()?;
    let mut args: Vec<String> = ["-x", "ir", "-target", "spir"].map(String::from).to_vec();
    for (k, o) in objects.iter().enumerate() {
        let path = dir.0.join(format!("{k}.bc"));
        std::fs::write(&path, o).map_err(|e| e.to_string())?;
        if k == 0 {
            args.push(path.display().to_string());
        } else {
            args.extend(["-Xclang", "-mlink-bitcode-file", "-Xclang"].map(String::from));
            args.push(path.display().to_string());
        }
    }
    if objects.is_empty() {
        return Err("no programs to link".into());
    }
    // Linked as bitcode (a library, or an executable's binary); an
    // executable then optimized as a built program is.
    args.extend(["-O2", "-Xclang", "-disable-llvm-passes", "-c", "-emit-llvm", "-o", "-"].map(String::from));
    let linked = run(clang, &args, &[])?;
    if library {
        return Ok(Linked::Library(linked));
    }
    let m = from_bitcode(cc, &linked)?;
    Ok(Linked::Executable(m, linked))
}

/// An executable's module from its bitcode (a linked program's binary).
pub fn from_bitcode(cc: &Compiler, bitcode: &[u8]) -> Result<Module, String> {
    let dir = Scratch::new()?;
    let path = dir.0.join("program.bc");
    std::fs::write(&path, bitcode).map_err(|e| e.to_string())?;
    let args: Vec<String> = ["-x", "ir", "-target", "spir", "-O2", "-mllvm", "-inline-threshold=100000", "-S", "-emit-llvm", "-o", "-"].map(String::from).into_iter().chain([path.display().to_string()]).chain(cc.builtin_args()).collect();
    let ir = run(&cc.clang, &args, &[])?;
    parse(&String::from_utf8(ir).map_err(|e| e.to_string())?)
}

fn run(clang: &Path, args: &[String], input: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new(clang)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't run {}: {e}", clang.display()))?;
    child.stdin.take().unwrap().write_all(input).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).replace("<stdin>", "program"));
    }
    Ok(out.stdout)
}

/// A directory of its own, removed when done.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Result<Scratch, String> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("wasm-env-cl-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_compile_with_headers_and_link() {
        let clang = test_clang();
        let header = ("lib/shared.h".to_string(), b"typedef struct { int a; float b; } pair;\nint twice(int x);\n".to_vec());
        let a = compile(&clang, "#include \"lib/shared.h\"\n__kernel void k(__global pair *p) { p->a = twice(p->a); }", "", std::slice::from_ref(&header), None).unwrap();
        let b = compile(&clang, "#include \"lib/shared.h\"\nint twice(int x) { return x * 2; }", "", &[header], None).unwrap();
        assert!(compile(&clang, "#include \"missing.h\"", "", &[], None).unwrap_err().contains("missing.h"));
        let Linked::Library(lib) = link(&clang, &[b], true).unwrap() else { panic!() };
        let Linked::Executable(m, _) = link(&clang, &[a, lib], false).unwrap() else { panic!() };
        // twice was inlined into the kernel.
        let k = m.function("k").unwrap();
        assert!(k.blocks.iter().flat_map(|b| &b.insts).all(|i| !matches!(&i.op, crate::ir::Op::Call(n, _) if n == "twice")));
    }
}

#[cfg(test)]
mod guest_tests {
    use super::*;

    #[test]
    fn a_program_s_paths_are_its_environment_s() {
        let root = Scratch::new().unwrap();
        std::fs::create_dir_all(root.0.join("opt/inc")).unwrap();
        std::fs::create_dir_all(root.0.join("work/local")).unwrap();
        std::fs::write(root.0.join("opt/inc/abs.h"), "#define ABS 3\n").unwrap();
        std::fs::write(root.0.join("work/local/rel.h"), "#define REL 4\n").unwrap();
        let guest = Guest { root: root.0.clone(), cwd: root.0.join("work") };
        let src = "#include \"/opt/inc/abs.h\"\n#include \"rel.h\"\n__kernel void k(__global int *p) { p[0] = ABS + REL; }";
        let m = build(&test_clang(), src, "-I local", Some(&guest)).unwrap();
        assert!(m.function("k").is_some());
        assert!(build(&test_clang(), src, "", None).is_err());
    }
}
