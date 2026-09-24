//! Translating arm64 ELF binaries to wasm-env programs on first use, and
//! caching the result (the Rosetta 2 model; see PLAN.md).
//!
//! When the runtime is asked to execute an ELF binary, it invokes the
//! `wasm-env-translate` tool to produce a WebAssembly module with the same
//! ABI as a recompiled program (docs/abi.md), so the rest of the runtime
//! runs it unchanged. Results are cached by content hash, so each binary is
//! translated once.
//!
//! Translation needs the toolchain (clang, wasm-ld) and the translator
//! binary, which are present in the dev shell. When they aren't, execution
//! of an ELF binary fails with ENOEXEC, exactly as before.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::abi::errno::*;
use crate::memory::SysResult;

/// A 64-bit FNV-1a hash of some bytes, for naming cache entries.
fn hash(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// The `wasm-env-translate` binary: `$WASM_ENV_TRANSLATE`, or next to the
/// running executable, or on `PATH`.
fn translator() -> PathBuf {
    if let Some(p) = std::env::var_os("WASM_ENV_TRANSLATE") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("wasm-env-translate");
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from("wasm-env-translate")
}

/// Where translated modules are cached.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("WASM_ENV_CACHE").map(PathBuf::from).or_else(|| {
        let var = |k| std::env::var_os(k).map(PathBuf::from);
        if cfg!(windows) {
            var("LOCALAPPDATA")
        } else if cfg!(target_os = "macos") {
            var("HOME").map(|h| h.join("Library/Caches"))
        } else {
            var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
        }
        .map(|d| d.join("wasm-env"))
    })?;
    Some(base.join("translated"))
}

/// The first bytes of a little-endian arm64 (EM_AARCH64 = 183) ELF.
pub fn is_arm64_elf(head: &[u8]) -> bool {
    head.len() >= 20
        && &head[..4] == b"\x7fELF"
        && head[4] == 2 // ELFCLASS64
        && head[5] == 1 // little-endian
        && u16::from_le_bytes([head[18], head[19]]) == 183
}

/// Returns the cached translated module for `elf`, translating it on the
/// first call. `trace` logs a line when translation actually runs.
pub fn ensure_translated(elf: &Path, trace: bool) -> SysResult<PathBuf> {
    let bytes = std::fs::read(elf).map_err(|e| crate::fs::errno(&e))?;
    // The cache key covers the binary and the translator's build, so a
    // rebuilt translator retranslates.
    let translator = translator();
    let tool_stamp = std::fs::metadata(&translator).and_then(|m| m.modified()).ok();
    let tool_hash = tool_stamp
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    let key = hash(&bytes) ^ hash(&tool_hash.to_le_bytes()) ^ bytes.len() as u64;

    let dir = cache_dir().ok_or(ENOENT)?;
    std::fs::create_dir_all(&dir).map_err(|e| crate::fs::errno(&e))?;
    let out = dir.join(format!("{key:016x}.wasm"));
    if out.exists() {
        return Ok(out);
    }

    if trace {
        eprintln!("[wasm-env] translating {}", elf.display());
    }
    // Translate to a unique temp file, then rename, so a concurrent run
    // never sees a half-written module.
    let tmp = dir.join(format!("{key:016x}.{}.tmp", std::process::id()));
    let mut cmd = Command::new(&translator);
    cmd.arg("translate").arg(elf).arg(&tmp);
    if !trace {
        // Keep the translator's diagnostics out of the program's output.
        cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    }
    let status = cmd.status().map_err(|_| ENOEXEC)?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(ENOEXEC);
    }
    std::fs::rename(&tmp, &out).map_err(|e| crate::fs::errno(&e))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_arm64_elf() {
        let mut head = vec![0u8; 20];
        head[..4].copy_from_slice(b"\x7fELF");
        head[4] = 2;
        head[5] = 1;
        head[18] = 183;
        assert!(is_arm64_elf(&head));
        head[18] = 62; // x86-64
        assert!(!is_arm64_elf(&head), "x86-64 isn't translated yet");
        assert!(!is_arm64_elf(b"\x7fELF"), "too short");
        assert!(!is_arm64_elf(b"\0asm\x01\0\0\0"), "wasm, not ELF");
    }

    #[test]
    fn hash_depends_on_contents() {
        assert_ne!(hash(b"abc"), hash(b"abd"));
        assert_eq!(hash(b"abc"), hash(b"abc"));
    }
}
