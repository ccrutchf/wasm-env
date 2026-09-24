//! Test fixtures: arm64 programs from tests/arm64 (and tests/c), built with
//! the cross compiler from the dev shell (`nix develop`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

pub const CROSS_CC: &str = "aarch64-unknown-linux-musl-gcc";

/// Fixtures already built in this test run. Tests run in parallel, so
/// building is serialized and each fixture is built once.
static BUILT: Mutex<Option<HashMap<String, Option<Vec<u8>>>>> = Mutex::new(None);

/// Builds tests/<source> for arm64 with `flags` and returns the binary's
/// bytes, or None (with a note) when the cross compiler isn't available.
pub fn fixture(source: &str, flags: &[&str]) -> Option<Vec<u8>> {
    let key = format!("{source} {}", flags.join(" "));
    let mut built = BUILT.lock().unwrap_or_else(|e| e.into_inner());
    let built = built.get_or_insert_with(HashMap::new);
    if let Some(bytes) = built.get(&key) {
        return bytes.clone();
    }
    let bytes = build(source, flags);
    built.insert(key, bytes.clone());
    bytes
}

fn build(source: &str, flags: &[&str]) -> Option<Vec<u8>> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let out_dir = repo.join("target/arm64-fixtures");
    std::fs::create_dir_all(&out_dir).unwrap();
    let name = format!("{}{}", source.replace('/', "_"), flags.join("").replace(['-', ' '], "_"));
    let out = out_dir.join(&name);
    // Build under a unique name and rename, so a concurrent test run never
    // sees a half-written file.
    let tmp = out_dir.join(format!("{name}.{}.tmp", std::process::id()));
    let status = Command::new(CROSS_CC).args(flags).arg("-o").arg(&tmp).arg(repo.join("tests").join(source)).status();
    match status {
        Ok(s) if s.success() => {
            std::fs::rename(&tmp, &out).unwrap();
            Some(std::fs::read(&out).unwrap())
        }
        Ok(s) => panic!("{CROSS_CC} failed on {source}: {s}"),
        Err(_) => {
            eprintln!("skipping: {CROSS_CC} not found (run inside `nix develop`)");
            None
        }
    }
}

/// The libc-free test program.
pub fn min() -> Option<Vec<u8>> {
    fixture("arm64/min.c", &["-O2", "-static", "-nostdlib", "-fno-stack-protector"])
}

/// A static musl program.
pub fn static_c(source: &str) -> Option<Vec<u8>> {
    fixture(source, &["-O2", "-static"])
}

/// Builds `source` as an arm64 executable linked against shared libraries
/// built from `lib_sources`, and returns the executable's bytes. Used to
/// test multi-object (dynamic) loading. None if the cross compiler is
/// unavailable.
pub fn fixture_linked(source: &str, flags: &[&str], lib_sources: &[&str]) -> Option<Vec<u8>> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let out_dir = repo.join("target/arm64-fixtures");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut libs = Vec::new();
    for lib in lib_sources {
        let so = out_dir.join(format!("{}.so", lib.replace('/', "_")));
        let status = std::process::Command::new(CROSS_CC)
            .args(["-O2", "-shared", "-nostdlib", "-fno-stack-protector", "-fPIC", "-o"])
            .arg(&so)
            .arg(repo.join("tests").join(lib))
            .status();
        match status {
            Ok(s) if s.success() => libs.push(so),
            Ok(s) => panic!("{CROSS_CC} failed on {lib}: {s}"),
            Err(_) => return None,
        }
    }
    let out = out_dir.join(format!("{}-linked", source.replace('/', "_")));
    let mut cmd = std::process::Command::new(CROSS_CC);
    cmd.args(flags).arg("-o").arg(&out).arg(repo.join("tests").join(source)).arg("-L").arg(&out_dir);
    for so in &libs {
        cmd.arg(so);
    }
    match cmd.status() {
        Ok(s) if s.success() => Some(std::fs::read(&out).unwrap()),
        Ok(s) => panic!("{CROSS_CC} link failed on {source}: {s}"),
        Err(_) => None,
    }
}
