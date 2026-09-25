//! Translating arm64 ELF binaries to wasm-env programs on first use, and
//! caching the result (the Rosetta 2 model; see PLAN.md).
//!
//! When the runtime is asked to execute an ELF binary, it invokes the
//! `wasm-env-translate` tool to produce a WebAssembly module with the same
//! ABI as a recompiled program (docs/abi.md), so the rest of the runtime
//! runs it unchanged. Results are cached by content hash, so each binary is
//! translated once.
//!
//! A dynamically-linked binary is translated together with its dynamic linker
//! and libraries, which the translator finds inside the environment's root
//! (`--root`). Libraries a program only `dlopen`s can't be found from the
//! binary, so the environment may list them in `/etc/wasm-env/dlopen`: lines of
//! `PROGRAM LIBRARY-OR-DIRECTORY...`, where PROGRAM is a file name pattern
//! (`*` matches anything) such as `python3*`. A cache entry records every file
//! the translation read, with its size and modification time, and is redone
//! if any of them changes.
//!
//! Translation needs the toolchain (clang, wasm-ld) and the translator
//! binary, which are present in the dev shell. When they aren't, execution
//! of an ELF binary fails with ENOEXEC, exactly as before.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::abi::errno::*;
use crate::memory::SysResult;

/// A 64-bit FNV-1a hash of some bytes, for naming cache entries.
pub fn hash(bytes: &[u8]) -> u64 {
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

/// Whether `name` matches a pattern where `*` matches any run of characters.
fn glob(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, rest)) => {
            let Some(tail) = name.strip_prefix(head) else { return false };
            (0..=tail.len()).filter(|&i| tail.is_char_boundary(i)).any(|i| glob(rest, &tail[i..]))
        }
    }
}

/// The libraries `/etc/wasm-env/dlopen` in `root` lists for a program named
/// `program` (a file name).
fn dlopen_list(root: &Path, program: &str) -> Vec<String> {
    let Ok(conf) = std::fs::read_to_string(root.join("etc/wasm-env/dlopen")) else { return Vec::new() };
    conf.lines()
        .map(|l| l.split('#').next().unwrap_or_default())
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let pattern = words.next()?;
            glob(pattern, program).then(|| words.map(String::from).collect::<Vec<_>>())
        })
        .flatten()
        .collect()
}

/// The first bytes of a little-endian arm64 (EM_AARCH64 = 183) ELF.
pub fn is_arm64_elf(head: &[u8]) -> bool {
    head.len() >= 20
        && &head[..4] == b"\x7fELF"
        && head[4] == 2 // ELFCLASS64
        && head[5] == 1 // little-endian
        && u16::from_le_bytes([head[18], head[19]]) == 183
}

/// A fingerprint of the files a translation read: one "size<TAB>mtime<TAB>path"
/// line each. A missing file fingerprints as "absent", so deleting a library
/// invalidates the entry too.
fn fingerprint(paths: &str) -> String {
    paths
        .lines()
        .filter(|l| !l.is_empty())
        .map(|p| match std::fs::metadata(p) {
            Ok(m) => {
                let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                format!("{}\t{}\t{p}\n", m.len(), t.map_or(0, |d| d.as_nanos()))
            }
            Err(_) => format!("absent\t\t{p}\n"),
        })
        .collect()
}

/// Whether a cache entry's recorded inputs are all unchanged.
fn still_valid(deps: &Path) -> bool {
    let Ok(recorded) = std::fs::read_to_string(deps) else { return false };
    let paths: String = recorded.lines().filter_map(|l| l.splitn(3, '\t').nth(2)).map(|p| format!("{p}\n")).collect();
    fingerprint(&paths) == recorded
}

/// Returns the cached translated module for `elf`, translating it on the
/// first call. Its dynamic linker and libraries come from `root`; `program`
/// is its file name, for `/etc/wasm-env/dlopen`. `trace` logs a line when
/// translation actually runs.
pub fn ensure_translated(elf: &Path, root: &Path, program: &str, trace: bool) -> SysResult<PathBuf> {
    let bytes = std::fs::read(elf).map_err(|e| crate::fs::errno(&e))?;
    // The cache key covers the binary and the translator's build, so a
    // rebuilt translator retranslates.
    let translator = translator();
    let tool_stamp = std::fs::metadata(&translator).and_then(|m| m.modified()).ok();
    let tool_hash = tool_stamp
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    let dlopen = dlopen_list(root, program);
    let root_hash = hash(root.as_os_str().as_encoded_bytes()) ^ hash(dlopen.join("\n").as_bytes()).rotate_left(3);
    let key = hash(&bytes) ^ hash(&tool_hash.to_le_bytes()).rotate_left(1) ^ root_hash.rotate_left(2) ^ bytes.len() as u64;

    let dir = cache_dir().ok_or(ENOENT)?;
    std::fs::create_dir_all(&dir).map_err(|e| crate::fs::errno(&e))?;
    let out = dir.join(format!("{key:016x}.wasm"));
    let deps = dir.join(format!("{key:016x}.deps"));
    if out.exists() && still_valid(&deps) {
        return Ok(out);
    }

    if trace {
        eprintln!("[wasm-env] translating {}", elf.display());
    }
    // Translate to a unique temp file, then rename, so a concurrent run
    // never sees a half-written module.
    let tmp = dir.join(format!("{key:016x}.{}.tmp", std::process::id()));
    let tmp_deps = dir.join(format!("{key:016x}.{}.deps.tmp", std::process::id()));
    let mut cmd = Command::new(&translator);
    cmd.arg("translate").arg(elf).arg(&tmp).arg("--root").arg(root).arg("--deps-out").arg(&tmp_deps);
    for lib in &dlopen {
        cmd.arg("--dlopen").arg(lib);
    }
    // Library bases from this program's last translation, whatever its
    // dlopen list was, so a changed list moves no library whose code is
    // already compiled (the translator's object cache).
    let layout = dir.join(format!(
        "{:016x}.layout",
        hash(elf.as_os_str().as_encoded_bytes()) ^ hash(root.as_os_str().as_encoded_bytes()).rotate_left(5)
    ));
    cmd.arg("--layout").arg(&layout);
    // Addresses earlier runs found missing (see record_missed_target).
    let roots = out.with_extension("roots");
    if roots.exists() {
        cmd.arg("--roots").arg(&roots);
    }
    if !trace {
        // Keep the translator's diagnostics out of the program's output.
        cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    }
    let status = cmd.status().map_err(|_| ENOEXEC)?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&tmp_deps);
        return Err(ENOEXEC);
    }
    // Record the inputs: the binary itself and whatever the translator read.
    let mut inputs = format!("{}\n{}\n", elf.display(), root.join("etc/wasm-env/dlopen").display());
    inputs.push_str(&std::fs::read_to_string(&tmp_deps).unwrap_or_default());
    let _ = std::fs::remove_file(&tmp_deps);
    std::fs::write(&deps, fingerprint(&inputs)).map_err(|e| crate::fs::errno(&e))?;
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

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn fingerprints_notice_changed_and_deleted_inputs() {
        let dir = std::env::temp_dir().join(format!("wasm-env-fp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (lib, deps) = (dir.join("lib with space.so"), dir.join("entry.deps"));
        std::fs::write(&lib, b"v1").unwrap();
        std::fs::write(&deps, fingerprint(&format!("{}\n", lib.display()))).unwrap();
        assert!(still_valid(&deps), "unchanged inputs");
        std::fs::write(&lib, b"version 2").unwrap();
        assert!(!still_valid(&deps), "a changed library");
        std::fs::write(&deps, fingerprint(&format!("{}\n", lib.display()))).unwrap();
        std::fs::remove_file(&lib).unwrap();
        assert!(!still_valid(&deps), "a deleted library");
        assert!(!still_valid(&dir.join("missing.deps")), "no record");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod dlopen_tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("python3*", "python3.12"));
        assert!(glob("python3*", "python3"));
        assert!(glob("*", "anything"));
        assert!(glob("a*c*e", "abcde"));
        assert!(!glob("python3*", "python"));
        assert!(!glob("greet", "greeter"));
    }

    #[test]
    fn reads_the_dlopen_list_for_a_program() {
        let root = std::env::temp_dir().join(format!("wasm-env-dlopen-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("etc/wasm-env")).unwrap();
        std::fs::write(
            root.join("etc/wasm-env/dlopen"),
            "# comment\npython3* /usr/lib/python3.12/lib-dynload\ngreet /usr/lib/libplugin.so /usr/lib/b.so # two\n",
        )
        .unwrap();
        assert_eq!(dlopen_list(&root, "python3.12"), ["/usr/lib/python3.12/lib-dynload"]);
        assert_eq!(dlopen_list(&root, "greet"), ["/usr/lib/libplugin.so", "/usr/lib/b.so"]);
        assert!(dlopen_list(&root, "ls").is_empty());
        assert!(dlopen_list(&root.join("nope"), "greet").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Records that a translated program jumped to code the translation missed
/// (an address computed at run time that static discovery didn't find),
/// as "object<TAB>offset" in the module's `.roots` file. For a cached
/// translation, the module is dropped, so the next exec retranslates with
/// these addresses as block roots: each run can only add real code, so the
/// translation converges.
pub fn record_missed_target(module: &Path, object: &str, offset: u64) {
    use std::io::Write;
    let roots = module.with_extension("roots");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&roots) {
        let _ = writeln!(f, "{object}\t{offset:#x}");
    }
    let cached = cache_dir().is_some_and(|d| module.parent() == Some(d.as_path()));
    if cached {
        let _ = std::fs::remove_file(module);
        eprintln!("wasm-env: recorded the missed address; the next run translates it");
    } else {
        eprintln!("wasm-env: recorded the missed address in {} (translate with --roots)", roots.display());
    }
}
