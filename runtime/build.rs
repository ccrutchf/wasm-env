//! Build script: a version for the translator's output, so translation
//! caches (the runtime's, and a converted image's) stay valid across
//! rebuilds that don't change the translator, and go stale when it does:
//! a hash of the translator's sources (see translate.rs, `tool_hash`).

use std::path::Path;

fn hash_tree(dir: &Path, h: &mut u64) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            hash_tree(&p, h);
        } else if let Ok(bytes) = std::fs::read(&p) {
            let name = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().into_owned();
            for b in name.bytes().chain(bytes) {
                *h = (*h ^ b as u64).wrapping_mul(0x100000001b3);
            }
        }
    }
}

fn main() {
    let translator = Path::new(env!("CARGO_MANIFEST_DIR")).join("../translator");
    let mut h = 0xcbf29ce484222325u64;
    for part in ["src", "support", "Cargo.toml"] {
        let p = translator.join(part);
        println!("cargo:rerun-if-changed={}", p.display());
        if p.is_dir() {
            hash_tree(&p, &mut h);
        } else if let Ok(bytes) = std::fs::read(&p) {
            for b in bytes {
                h = (h ^ b as u64).wrapping_mul(0x100000001b3);
            }
        }
    }
    println!("cargo:rustc-env=WASM_ENV_TRANSLATOR_VERSION={h:016x}");
}
