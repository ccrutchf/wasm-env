//! OpenCL C to LLVM IR, by clang (the toolchain translation uses): the
//! spir target (32-bit pointers and size_t, which suit WGSL's u32), and
//! everything inlined into the kernels.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::ir::{Module, Res, parse};

/// A program's source compiled: its module, or clang's messages (the build
/// log) on failure.
pub fn build(clang: &Path, source: &str, options: &str) -> Result<Module, String> {
    let ir = clang_ir(clang, source, options)?;
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

pub fn clang_ir(clang: &Path, source: &str, options: &str) -> Res<String> {
    let mut args: Vec<String> = ["-x", "cl", "-cl-std=CL1.2", "-target", "spir", "-O2", "-emit-llvm", "-S", "-Xclang", "-finclude-default-header", "-cl-kernel-arg-info"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // Everything inlined: the WGSL is one function per kernel.
    args.extend(["-mllvm", "-inline-threshold=100000"].map(String::from));
    args.extend(passed(options));
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
pub(crate) fn test_clang() -> std::path::PathBuf {
    std::env::var_os("WASM_CC").map_or_else(|| "clang".into(), Into::into)
}
