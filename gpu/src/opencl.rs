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
    build_with_ir(cc, source, options, guest).map(|(m, _)| m)
}

/// `build`, and the module's IR: the program's binary, which `from_bitcode`
/// builds again (clang reads IR as text as well as bitcode).
pub fn build_with_ir(cc: &Compiler, source: &str, options: &str, guest: Option<&Guest>) -> Result<(Module, String), String> {
    let ir = clang_ir(cc, source, options, guest)?;
    Ok((parse(&ir)?, ir))
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

/// The extensions the device has (packages/opencl/icd.c reports the same):
/// clang defines their macros and no others (for spir it would define
/// every extension's, and programs test them: dlprim takes its Intel-only
/// path where `cl_intel_subgroups` is defined).
const EXTENSIONS: &str = "-cl-ext=-all,+cl_khr_global_int32_base_atomics,+cl_khr_global_int32_extended_atomics,\
+cl_khr_local_int32_base_atomics,+cl_khr_local_int32_extended_atomics,+cl_khr_byte_addressable_store,+cl_khr_fp64,+cl_khr_fp16,\
+cl_khr_3d_image_writes";

pub fn clang_ir(cc: &Compiler, source: &str, options: &str, guest: Option<&Guest>) -> Res<String> {
    let clang = &cc.clang;
    let dir = Scratch::new()?;
    // (__OPENCL_VERSION__ is
    // the device's, which clang leaves to the driver.)
    let mut args: Vec<String> = ["-x", "cl", "-cl-std=CL1.2", "-target", "spir", "-O2", "-emit-llvm", "-S", "-Xclang", "-finclude-default-header", "-Xclang", EXTENSIONS, "-cl-kernel-arg-info", "-D__OPENCL_VERSION__=120"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // Everything inlined: the WGSL is one function per kernel.
    args.extend(["-mllvm", "-inline-threshold=100000"].map(String::from));
    args.extend(extra_builtins(&dir)?);
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
    let ir = String::from_utf8(out.stdout).map_err(|e| e.to_string())?;
    with_images(cc, ir)
}

#[cfg(test)]
pub(crate) fn test_clang() -> Compiler {
    let clang = std::env::var_os("WASM_CC").map_or_else(|| "clang".into(), Into::into);
    // (The builtins too, if packages/libclc/build.sh has built them.)
    let builtins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../build/libclc/clspv--.bc");
    // (Built on Linux and run under Wine, tests/cross.sh: the path is
    // Linux's, Wine's Z: drive to Windows' clang.)
    #[cfg(windows)]
    let builtins = if builtins.starts_with("/") { std::path::PathBuf::from(format!("Z:{}", builtins.display())) } else { builtins };
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
    let mut args: Vec<String> = ["-x", "cl", "-cl-std=CL1.2", "-target", "spir", "-O2", "-Xclang", "-disable-llvm-passes", "-Xclang", EXTENSIONS, "-c", "-emit-llvm"]
        .iter()
        .chain(&["-Xclang", "-finclude-default-header", "-cl-kernel-arg-info", "-D__OPENCL_VERSION__=120"])
        .map(|s| s.to_string())
        .collect();
    args.extend(["-I".to_string(), dir.0.display().to_string()]);
    args.extend(extra_builtins(&dir)?);
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
    parse(&with_images(cc, String::from_utf8(ir).map_err(|e| e.to_string())?)?)
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

/// Builtins the device has beyond OpenCL C 1.2's, declared for programs
/// built as 1.2: 2.0's ctz (the conformance tests use it on 1.2 devices too).
const EXTRA_BUILTINS: &str = "#if __OPENCL_C_VERSION__ < 200
#define __WENV_D(T) T __attribute__((overloadable, const)) ctz(T);
#define __WENV_V(T) __WENV_D(T) __WENV_D(T##2) __WENV_D(T##3) __WENV_D(T##4) __WENV_D(T##8) __WENV_D(T##16)
__WENV_V(char) __WENV_V(uchar) __WENV_V(short) __WENV_V(ushort) __WENV_V(int) __WENV_V(uint) __WENV_V(long) __WENV_V(ulong)
#undef __WENV_V
#undef __WENV_D
#endif
";

/// clang's arguments including `EXTRA_BUILTINS` and `vector_loops` (written
/// into `dir`).
fn extra_builtins(dir: &Scratch) -> Result<Vec<String>, String> {
    let path = dir.0.join("wasm-env-builtins.h");
    std::fs::write(&path, format!("{EXTRA_BUILTINS}{}", vector_loops())).map_err(|e| e.to_string())?;
    Ok(vec!["-include".into(), path.display().to_string()])
}

// --- Images -------------------------------------------------------------------
//
// An image is a buffer of its pixels; its header (its size, pitches and
// format) comes with the image argument, in the argument block (see
// images.cl, wgsl.rs and packages/opencl/icd.c). A kernel compiled for
// spir has image and sampler handles (`target("spirv.Image", ...)`,
// `target("spirv.Sampler")`) and calls the image builtins, which nothing
// defines. A program that uses them is compiled again: its handles become a
// pointer to the image's buffer and the sampler's bits, its builtin calls
// calls to images.cl's functions, which are linked in and inlined.

const IMAGES_CL: &str = include_str!("images.cl");

/// Whether a module's IR uses images or samplers.
fn uses_images(ir: &str) -> bool {
    ir.contains("target(\"spirv.Image\"") || ir.contains("target(\"spirv.Sampler\")")
}

/// The images.cl function for an image builtin's mangled name (after `@`, up
/// to its `(`), or None if it isn't one: `__img_<builtin>_<image type>`, then
/// for reads `_s` (with a sampler) or `_n`, and `_i` or `_f` for the
/// coordinates' type; for writes `_i`.
fn image_function(mangled: &str) -> Option<String> {
    // A length-prefixed name at `s`: (name, rest).
    fn source_name(s: &str) -> Option<(&str, &str)> {
        let digits = s.bytes().take_while(u8::is_ascii_digit).count();
        let n: usize = s[..digits].parse().ok()?;
        let rest = &s[digits..];
        Some((rest.get(..n)?, &rest[n..]))
    }
    let (name, rest) = source_name(mangled.strip_prefix("_Z")?)?;
    if !(name.starts_with("read_image") || name.starts_with("write_image") || name.starts_with("get_image_")) {
        return None;
    }
    let (kind, rest) = source_name(rest)?;
    let kind = kind.strip_prefix("ocl_")?;
    if !kind.starts_with("image") {
        return None;
    }
    // (The coordinates' type: an int or float scalar or vector.)
    let coords = |rest: &str| {
        let t = rest.strip_prefix("Dv").map(|r| r.split_once('_').map_or("", |(_, t)| t)).unwrap_or(rest);
        if t.starts_with('f') { "f" } else { "i" }
    };
    Some(if name.starts_with("read_image") {
        let (sampled, rest) = match rest.strip_prefix("11ocl_sampler") {
            Some(r) => ("s", r),
            None => ("n", rest),
        };
        format!("__img_{name}_{kind}_{sampled}_{}", coords(rest))
    } else if name.starts_with("write_image") {
        format!("__img_{name}_{kind}_i")
    } else if name == "get_image_dim" {
        let n = if kind.starts_with("image3d") { 4 } else { 2 };
        format!("__img_get_image_dim{n}_{kind}")
    } else {
        format!("__img_{name}_{kind}")
    })
}

/// A module's IR with its image and sampler handles as a pointer and the
/// sampler's bits, and its image builtins images.cl's.
fn rewrite_images(ir: &str) -> String {
    let mut out = String::with_capacity(ir.len());
    let mut rest = ir;
    while let Some(at) = rest.find("target(\"spirv.") {
        out.push_str(&rest[..at]);
        let end = rest[at..].find(')').map_or(rest.len(), |e| at + e + 1);
        let ty = &rest[at..end];
        out.push_str(if ty.starts_with("target(\"spirv.Sampler\"") { "i32" } else { "ptr addrspace(1)" });
        rest = &rest[end..];
    }
    out.push_str(rest);
    let ir = out;
    let mut out = String::with_capacity(ir.len());
    let mut rest = ir.as_str();
    while let Some(at) = rest.find("@_Z") {
        out.push_str(&rest[..at + 1]);
        let name_end = rest[at + 1..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(rest.len(), |e| at + 1 + e);
        let name = &rest[at + 1..name_end];
        match image_function(name) {
            Some(f) => out.push_str(&f),
            None => out.push_str(name),
        }
        rest = &rest[name_end..];
    }
    out.push_str(rest);
    out
}

/// images.cl as LLVM bitcode, compiled once per process.
fn images_bitcode(cc: &Compiler) -> Result<std::path::PathBuf, String> {
    static BUILT: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
    let mut built = BUILT.lock().unwrap();
    if let Some(p) = built.as_ref().filter(|p| p.exists()) {
        return Ok(p.clone());
    }
    let path = std::env::temp_dir().join(format!("wasm-env-images-{}.bc", std::process::id()));
    let args: Vec<String> = ["-x", "cl", "-cl-std=CL1.2", "-target", "spir", "-O2", "-emit-llvm", "-c", "-Xclang", "-finclude-default-header", "-o"]
        .map(String::from)
        .into_iter()
        .chain([path.display().to_string(), "-".into()])
        .collect();
    run(&cc.clang, &args, IMAGES_CL.as_bytes()).map_err(|e| format!("images.cl: {e}"))?;
    *built = Some(path.clone());
    Ok(path)
}

/// A module's optimized IR, with its images (if it uses any) as buffers.
fn with_images(cc: &Compiler, ir: String) -> Res<String> {
    if !uses_images(&ir) {
        return Ok(ir);
    }
    let dir = Scratch::new()?;
    let path = dir.0.join("program.ll");
    std::fs::write(&path, rewrite_images(&ir)).map_err(|e| e.to_string())?;
    let images = images_bitcode(cc)?;
    let args: Vec<String> = ["-x", "ir", "-target", "spir", "-O2", "-mllvm", "-inline-threshold=100000", "-S", "-emit-llvm", "-o", "-"]
        .map(String::from)
        .into_iter()
        .chain([path.display().to_string(), "-Xclang".into(), "-mlink-builtin-bitcode".into(), "-Xclang".into(), images.display().to_string()])
        .chain(cc.builtin_args())
        .collect();
    let ir = run(&cc.clang, &args, &[])?;
    String::from_utf8(ir).map_err(|e| e.to_string())
}

#[cfg(test)]
mod extension_tests {
    use super::*;

    #[test]
    fn only_the_devices_extensions_are_defined() {
        // A program sees the macros of the extensions the device has, and
        // not those of others (cl_intel_subgroups: dlprim's test).
        let src = "#if !defined(cl_khr_fp64) || !defined(cl_khr_fp16) || !defined(cl_khr_global_int32_base_atomics)\n#error missing\n#endif\n\
                   #if defined(cl_intel_subgroups) || defined(cl_khr_subgroups) || defined(cl_khr_int64_base_atomics)\n#error extra\n#endif\n\
                   __kernel void k(__global int *p) { p[0] = 1; }";
        clang_ir(&test_clang(), src, "", None).unwrap();
    }

    #[test]
    fn a_built_programs_ir_builds_again() {
        // Its binary (clGetProgramInfo) is its IR: built again without the
        // options it was built with (a -D the source needs), as dlprim does.
        let src = "__kernel void k(__global int *p) { p[get_global_id(0)] = SCALE * 2; }";
        let (m, ir) = build_with_ir(&test_clang(), src, "-DSCALE=3", None).unwrap();
        let again = from_bitcode(&test_clang(), ir.as_bytes()).unwrap();
        let kernels = |m: &Module| m.functions.iter().filter(|f| f.kernel).map(|f| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(kernels(&again), kernels(&m));
        assert_eq!(kernels(&again), ["k"]);
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn image_builtins_are_renamed() {
        assert_eq!(image_function("_Z11read_imagef14ocl_image2d_ro11ocl_samplerDv2_f").as_deref(), Some("__img_read_imagef_image2d_ro_s_f"));
        assert_eq!(image_function("_Z12read_imageui20ocl_image2d_array_roDv4_i").as_deref(), Some("__img_read_imageui_image2d_array_ro_n_i"));
        assert_eq!(image_function("_Z11read_imagei14ocl_image1d_ro11ocl_sampleri").as_deref(), Some("__img_read_imagei_image1d_ro_s_i"));
        assert_eq!(image_function("_Z12write_imagef14ocl_image3d_woDv4_iDv4_f").as_deref(), Some("__img_write_imagef_image3d_wo_i"));
        assert_eq!(image_function("_Z13get_image_dim14ocl_image3d_ro").as_deref(), Some("__img_get_image_dim4_image3d_ro"));
        assert_eq!(image_function("_Z15get_image_width20ocl_image1d_array_wo").as_deref(), Some("__img_get_image_width_image1d_array_wo"));
        assert_eq!(image_function("_Z13get_global_idj"), None);
    }

    #[test]
    fn every_image_builtin_resolves() {
        // Each image type, access and builtin form, compiled: no image
        // handle or builtin is left (other builtins are WGSL's, see wgsl.rs).
        let mut src = String::from("const sampler_t s = CLK_NORMALIZED_COORDS_TRUE | CLK_ADDRESS_REPEAT | CLK_FILTER_LINEAR;\n");
        for (t, ci, cf) in [("image1d_t", "int", "float"), ("image1d_array_t", "int2", "float2"), ("image2d_t", "int2", "float2"), ("image2d_array_t", "int4", "float4"), ("image3d_t", "int4", "float4")] {
            let n = t.trim_end_matches("_t");
            src.push_str(&format!(
                "__kernel void k_{n}(read_only {t} r, write_only {t} w, sampler_t s2, __global float4 *o, __global int *d) {{
    {ci} c = ({ci})(1); {cf} f = ({cf})(0.5f);
    o[0] = read_imagef(r, s, f) + read_imagef(r, s2, c) + read_imagef(r, c) + convert_float4(read_imagei(r, s, f)) + convert_float4(read_imageui(r, c));
    write_imagef(w, c, o[1]); write_imagei(w, c, (int4)(1)); write_imageui(w, c, (uint4)(2));
    d[0] = get_image_width(r) + get_image_channel_order(w) + get_image_channel_data_type(r);
}}\n"
            ));
        }
        let ir = clang_ir(&test_clang(), &src, "", None).unwrap();
        assert!(!ir.contains("spirv."), "image handles left");
        for line in ir.lines().filter(|l| l.starts_with("declare")) {
            assert!(line.contains("__wenv_image_header") || (!line.contains("image") && !line.contains("sampler")), "unresolved: {line}");
        }
        // And each translates (the headers' words read from the block).
        let m = build(&test_clang(), &src, "", None).unwrap();
        let launch = crate::wgsl::Launch { local: [1, 1, 1], local_bytes: vec![], ..Default::default() };
        for n in ["image1d", "image1d_array", "image2d", "image2d_array", "image3d"] {
            crate::wgsl::compile(&m, &format!("k_{n}"), &launch, Default::default()).unwrap_or_else(|e| panic!("k_{n}: {e}"));
        }
    }
}

/// Math builtins (libclc's functions, each tens to hundreds of operations;
/// a double's hundreds more in software, see f64.rs) on vectors, as a loop
/// over the lanes: one copy of the function in the kernel rather than one
/// per lane. GPU drivers inline every function call (Mesa does), so it
/// takes a loop to keep a double16 `sin` from being sixteen `sin`s, which
/// lavapipe takes minutes to compile, and a float16 `sin` (software fma on
/// Intel's GPU) from taking its compiler over 24 GB. Doubles of every
/// width, floats and halves of 8 and 16 lanes. A macro per builtin calls an
/// overload that loops for those and calls the builtin itself for the rest.
fn vector_loops() -> String {
    const UNARY: &[&str] = &[
        "acos", "acosh", "acospi", "asin", "asinh", "asinpi", "atan", "atanh", "atanpi", "cbrt", "cos", "cosh", "cospi", "erf", "erfc", "exp", "exp2",
        "exp10", "expm1", "lgamma", "log", "log10", "log1p", "log2", "logb", "rsqrt", "sin", "sinh", "sinpi", "sqrt", "tan", "tanh", "tanpi", "tgamma",
    ];
    const BINARY: &[&str] = &["atan2", "atan2pi", "fmod", "hypot", "pow", "powr", "remainder"];
    // (With an int second argument.)
    const INT: &[&str] = &["pown", "rootn"];
    const TERNARY: &[&str] = &["fma"];
    let widths = ["2", "3", "4", "8", "16"];
    // (Vectors looped over: element type and width.)
    let looped: Vec<(&str, &str)> = widths.iter().map(|&n| ("double", n)).chain(["float", "half"].iter().flat_map(|&e| ["8", "16"].map(|n| (e, n)))).collect();
    let others: Vec<String> = ["float", "half", "double"]
        .iter()
        .flat_map(|t| std::iter::once(t.to_string()).chain(widths.iter().map(move |n| format!("{t}{n}"))))
        .filter(|t| !looped.iter().any(|(e, n)| *t == format!("{e}{n}")))
        .collect();
    let mut s = String::from(
        "#pragma OPENCL EXTENSION cl_khr_fp64 : enable\n#pragma OPENCL EXTENSION cl_khr_fp16 : enable\n\
         #if __OPENCL_C_VERSION__ >= 120\n#define __WENV_F static __attribute__((overloadable, always_inline))\n\
         #else\n#define __WENV_F __attribute__((overloadable, always_inline))\n#endif\n",
    );
    // An argument's lane k: vectors as arrays in private memory.
    let lane = |v: &str, t: &str| format!("((const {t} *)&{v})[k]");
    for (names, arity) in [(UNARY, 1), (BINARY, 2), (INT, 0), (TERNARY, 3)] {
        for f in names {
            let params = |t: &str, i: &str| match arity {
                1 => format!("{t} x"),
                2 => format!("{t} x, {t} y"),
                3 => format!("{t} x, {t} y, {t} z"),
                _ => format!("{t} x, {i} y"),
            };
            let args = match arity {
                1 => "x",
                2 => "x, y",
                3 => "x, y, z",
                _ => "x, y",
            };
            for t in &others {
                let i = t.replace("float", "int").replace("half", "int").replace("double", "int");
                s.push_str(&format!("__WENV_F {t} __wenv_{f}({}) {{ return ({f})({args}); }}\n", params(t, &i)));
            }
            for &(e, n) in &looped {
                let t = format!("{e}{n}");
                let i = format!("int{n}");
                let call = match arity {
                    1 => format!("({f})({})", lane("x", e)),
                    2 => format!("({f})({}, {})", lane("x", e), lane("y", e)),
                    3 => format!("({f})({}, {}, {})", lane("x", e), lane("y", e), lane("z", e)),
                    _ => format!("({f})({}, {})", lane("x", e), lane("y", "int")),
                };
                s.push_str(&format!(
                    "__WENV_F {t} __wenv_{f}({}) {{ {t} r; __attribute__((opencl_unroll_hint(1))) for (int k = 0; k < {n}; k++) (({e} *)&r)[k] = {call}; return r; }}\n",
                    params(&t, &i)
                ));
            }
            let ps = match arity {
                1 => "x",
                2 | 0 => "x, y",
                _ => "x, y, z",
            };
            s.push_str(&format!("#define {f}({ps}) __wenv_{f}({ps})\n"));
        }
    }
    // With a pointer for a second result, in any address space: lgamma_r's
    // sign, sincos's cosine, remquo's quotient bits.
    for (f, int_result, binary) in [("lgamma_r", true, false), ("sincos", false, false), ("remquo", true, true)] {
        for space in ["__global", "__local", "__private"] {
            let params = |t: &str, p: &str| if binary { format!("{t} x, {t} y, {space} {p} *q") } else { format!("{t} x, {space} {p} *q") };
            let args = if binary { "x, y, q" } else { "x, q" };
            for t in &others {
                let i = t.replace("float", "int").replace("half", "int").replace("double", "int");
                let p = if int_result { i } else { t.clone() };
                s.push_str(&format!("__WENV_F {t} __wenv_{f}({}) {{ return ({f})({args}); }}\n", params(t, &p)));
            }
            for &(el, n) in &looped {
                let t = format!("{el}{n}");
                let (p, pe) = if int_result { (format!("int{n}"), "int") } else { (t.clone(), el) };
                let y = if binary { format!(", {}", lane("y", el)) } else { String::new() };
                s.push_str(&format!(
                    "__WENV_F {t} __wenv_{f}({}) {{ {t} r; __attribute__((opencl_unroll_hint(1))) for (int k = 0; k < {n}; k++) {{ {pe} e; (({el} *)&r)[k] = ({f})({}{y}, &e); (({space} {pe} *)q)[k] = e; }} return r; }}\n",
                    params(&t, &p),
                    lane("x", el)
                ));
            }
        }
        let ps = if binary { "x, y, q" } else { "x, q" };
        s.push_str(&format!("#define {f}({ps}) __wenv_{f}({ps})\n"));
    }
    s
}

#[cfg(test)]
mod loop_tests {
    use super::*;

    #[test]
    fn the_loops_build_for_opencl_c_1_1() {
        // (Which has no `static`: the header's functions are plain there.)
        let src = "#pragma OPENCL EXTENSION cl_khr_fp64 : enable\n__kernel void k(__global double4 *p) { p[0] = sin(p[1]); }";
        build(&test_clang(), src, "-cl-std=CL1.1", None).unwrap();
    }

    #[test]
    fn wide_float_and_half_math_is_a_loop() {
        let src = "#pragma OPENCL EXTENSION cl_khr_fp16 : enable
__kernel void k(__global float16 *o, __global half16 *h) { o[0] = sin(o[1]); h[0] = sin(h[1]); }";
        let ir = clang_ir(&test_clang(), src, "", None).unwrap();
        assert!(ir.lines().filter(|l| l.contains("!llvm.loop")).count() >= 2);
        let scalars = clang_ir(
            &test_clang(),
            "#pragma OPENCL EXTENSION cl_khr_fp16 : enable
__kernel void k(__global float *o, __global half *h) { o[0] = sin(o[1]); h[0] = sin(h[1]); }",
            "",
            None,
        )
        .unwrap();
        assert!(ir.len() < 2 * scalars.len(), "{} bytes of IR, {} for scalars", ir.len(), scalars.len());
    }

    #[test]
    fn double_vector_math_is_a_loop() {
        // One copy of each function, in a loop over the lanes: including those
        // with a second result through a pointer, in each address space.
        let src = "#pragma OPENCL EXTENSION cl_khr_fp64 : enable
__kernel void k(__global double16 *o, __global int16 *s, __global double8 *c, __local int4 *q) {
    o[0] = sin(o[1]);
    o[2] = lgamma_r(o[3], s);
    double8 cv; c[0] = sincos(c[1], &cv); c[2] = cv;
    ((__global double4 *)o)[20] = remquo(((__global double4 *)o)[21], ((__global double4 *)o)[22], q);
    s[1].s0123 = q[0];
}";
        let ir = clang_ir(&test_clang(), src, "", None).unwrap();
        let loops = ir.lines().filter(|l| l.contains("!llvm.loop")).count();
        assert!(loops >= 4, "{loops} loops");
        // About as large as the same on scalars (with the lanes, it would be
        // many times larger).
        let scalars = clang_ir(
            &test_clang(),
            "#pragma OPENCL EXTENSION cl_khr_fp64 : enable
__kernel void k(__global double *o, __global int *s, __local int *q) { double c; o[0] = sin(o[1]); o[2] = lgamma_r(o[3], s); o[4] = sincos(o[5], &c); o[6] = c; o[7] = remquo(o[8], o[9], q); s[1] = q[0]; }",
            "",
            None,
        )
        .unwrap();
        assert!(ir.len() < 2 * scalars.len(), "{} bytes of IR, {} for scalars", ir.len(), scalars.len());
    }
}
