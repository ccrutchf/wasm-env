//! Finding what a dynamically-linked program needs inside a root
//! filesystem: its dynamic linker (PT_INTERP) and its shared libraries
//! (DT_NEEDED, recursively, breadth-first), searched the way the dynamic
//! linker will search at run time. The guest paths found here are the ones
//! the translated dynamic linker later opens, which is how the support code
//! recognizes a library it must place at its prelinked address.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use object::Endianness;
use object::elf;
use object::read::elf::{ElfFile64, ProgramHeader};

pub struct Dep {
    /// The path the dynamic linker will open, inside the root.
    pub guest: String,
    /// The same file on the host.
    pub host: PathBuf,
}

pub struct Deps {
    pub interp: Option<Dep>,
    pub libs: Vec<Dep>,
}

/// The dynamic-linking facts of one ELF object.
#[derive(Default)]
struct DynInfo {
    interp: Option<String>,
    needed: Vec<String>,
    /// DT_RPATH and DT_RUNPATH directories, colon-separated entries split.
    rpath: Vec<String>,
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn dyn_info(bytes: &[u8]) -> Result<DynInfo> {
    let file = ElfFile64::<Endianness>::parse(bytes).context("not a 64-bit ELF file")?;
    let endian = file.endian();
    let mut info = DynInfo::default();
    let mut loads = Vec::new();
    let mut dynamic: &[u8] = &[];
    for ph in file.elf_program_headers() {
        let data = || ph.data(endian, bytes).map_err(|_| anyhow::anyhow!("bad program header"));
        match ph.p_type(endian) {
            elf::PT_INTERP => info.interp = Some(cstr(data()?)),
            elf::PT_DYNAMIC => dynamic = data()?,
            elf::PT_LOAD => loads.push((ph.p_vaddr(endian), ph.p_offset(endian), ph.p_filesz(endian))),
            _ => {}
        }
    }
    // DT_STRTAB is an address; find it in the file through the segments.
    let file_offset = |vaddr: u64| {
        loads.iter().find(|&&(va, _, sz)| vaddr >= va && vaddr < va + sz).map(|&(va, off, _)| (off + vaddr - va) as usize)
    };
    let (mut strtab, mut needed, mut paths) = (None, Vec::new(), Vec::new());
    for entry in dynamic.as_chunks::<16>().0 {
        let tag = u64::from_le_bytes(entry[..8].try_into().unwrap()) as u32;
        let val = u64::from_le_bytes(entry[8..].try_into().unwrap());
        match tag {
            elf::DT_NULL => break,
            elf::DT_STRTAB => strtab = file_offset(val),
            elf::DT_NEEDED => needed.push(val as usize),
            elf::DT_RPATH | elf::DT_RUNPATH => paths.push(val as usize),
            _ => {}
        }
    }
    if let Some(strtab) = strtab {
        let string = |off: usize| bytes.get(strtab + off..).map(cstr).unwrap_or_default();
        info.needed = needed.into_iter().map(string).collect();
        info.rpath = paths.into_iter().map(string).flat_map(|p| p.split(':').map(String::from).collect::<Vec<_>>()).collect();
    }
    Ok(info)
}

/// The host path of a guest path inside `root`.
fn host(root: &Path, guest: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(guest.split('/').filter(|c| !c.is_empty()));
    p
}

/// Resolves `path` inside `root`, following symlinks as if `root` were `/`
/// (absolute link targets stay inside it). None if it doesn't exist.
pub fn realpath(root: &Path, path: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut todo: Vec<String> = path.split('/').filter(|c| !c.is_empty()).rev().map(String::from).collect();
    let mut links = 0;
    while let Some(c) = todo.pop() {
        match c.as_str() {
            "." => continue,
            ".." => {
                parts.pop();
                continue;
            }
            _ => parts.push(c),
        }
        let h = host(root, &parts.join("/"));
        let meta = std::fs::symlink_metadata(&h).ok()?;
        if meta.file_type().is_symlink() {
            links += 1;
            if links > 40 {
                return None;
            }
            let target = std::fs::read_link(&h).ok()?;
            let target = target.to_str()?;
            parts.pop();
            if target.starts_with('/') {
                parts.clear();
            }
            todo.extend(target.split('/').filter(|c| !c.is_empty()).rev().map(String::from));
        }
    }
    Some(format!("/{}", parts.join("/")))
}

/// musl's dynamic linker is also libc, and never loads these names: they
/// all mean itself (see load_library in musl's ldso/dynlink.c).
fn musl_reserved(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("lib") else { return false };
    ["c", "pthread", "rt", "m", "dl", "util", "xnet"].iter().any(|z| {
        rest.strip_prefix(z).is_some_and(|tail| tail.is_empty() || tail.starts_with(".so"))
    })
}

/// The dynamic linker's default search path: musl reads
/// /etc/ld-musl-<arch>.path (colon- or newline-separated) if present; glibc
/// uses its multiarch directories.
fn system_path(root: &Path, musl: bool) -> Vec<String> {
    if musl {
        if let Ok(s) = std::fs::read_to_string(host(root, "/etc/ld-musl-aarch64.path")) {
            return s.split([':', '\n']).filter(|p| !p.is_empty()).map(String::from).collect();
        }
        return ["/lib", "/usr/local/lib", "/usr/lib"].map(String::from).to_vec();
    }
    ["/lib/aarch64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/lib", "/usr/lib"].map(String::from).to_vec()
}

/// Finds the interpreter and every shared library `exe` needs inside `root`,
/// plus `dlopen` (guest paths of libraries the program loads itself, or
/// directories of them) and everything those need.
pub fn resolve(root: &Path, exe: &[u8], dlopen: &[String]) -> Result<Deps> {
    let info = dyn_info(exe)?;
    let Some(interp_path) = info.interp.clone() else {
        return Ok(Deps { interp: None, libs: Vec::new() });
    };
    let interp_real =
        realpath(root, &interp_path).with_context(|| format!("dynamic linker {interp_path} not found in the root"))?;
    let musl = interp_path.contains("ld-musl");
    let sys = system_path(root, musl);

    let mut seen: HashSet<String> = HashSet::from([interp_real.clone()]);
    let mut libs = Vec::new();
    // (needed name, the requester's search directories), breadth-first.
    let mut queue: VecDeque<(String, Vec<String>)> = info.needed.iter().map(|n| (n.clone(), info.rpath.clone())).collect();
    for p in dlopen {
        let real = realpath(root, p).with_context(|| format!("{p} not found in the root"))?;
        let h = host(root, &real);
        if h.is_dir() {
            let mut names: Vec<String> = std::fs::read_dir(&h)?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".so"))
                .collect();
            names.sort();
            queue.extend(names.into_iter().map(|n| (format!("{}/{n}", p.trim_end_matches('/')), Vec::new())));
        } else {
            queue.push_back((p.clone(), Vec::new()));
        }
    }
    while let Some((name, rpath)) = queue.pop_front() {
        if musl && musl_reserved(&name) {
            continue;
        }
        let found = if name.contains('/') {
            realpath(root, &name).map(|r| (name.clone(), r))
        } else {
            rpath
                .iter()
                .filter(|d| !d.contains("$ORIGIN"))
                .chain(sys.iter())
                .find_map(|dir| {
                    let guest = format!("{}/{name}", dir.trim_end_matches('/'));
                    realpath(root, &guest).filter(|r| host(root, r).is_file()).map(|r| (guest, r))
                })
        };
        let Some((guest, real)) = found else { bail!("library {name} not found in the root") };
        if !seen.insert(real.clone()) {
            continue;
        }
        let bytes = std::fs::read(host(root, &real)).with_context(|| format!("reading {real}"))?;
        let dep_info = dyn_info(&bytes).with_context(|| format!("reading {real}"))?;
        for n in dep_info.needed {
            queue.push_back((n, dep_info.rpath.clone()));
        }
        libs.push(Dep { guest, host: host(root, &real) });
    }
    Ok(Deps { interp: Some(Dep { guest: interp_path, host: host(root, &interp_real) }), libs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn musl_reserved_names_mean_the_dynamic_linker() {
        for n in ["libc.so", "libm.so.6", "libpthread.so.0", "libdl.so.2", "libc.so"] {
            assert!(musl_reserved(n), "{n}");
        }
        for n in ["libcrypto.so.3", "libz.so.1", "libmagic.so", "c.so"] {
            assert!(!musl_reserved(n), "{n}");
        }
    }

    #[test]
    fn realpath_follows_symlinks_inside_the_root() {
        let root = std::env::temp_dir().join(format!("wasm-env-deps-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::create_dir_all(root.join("usr/lib")).unwrap();
        std::fs::write(root.join("lib/ld-musl-aarch64.so.1"), b"x").unwrap();
        // Absolute and relative links, resolved against the root, not /.
        std::os::unix::fs::symlink("/lib/ld-musl-aarch64.so.1", root.join("usr/lib/libc.so")).unwrap();
        std::os::unix::fs::symlink("../../lib/ld-musl-aarch64.so.1", root.join("usr/lib/rel.so")).unwrap();
        assert_eq!(realpath(&root, "/usr/lib/libc.so").as_deref(), Some("/lib/ld-musl-aarch64.so.1"));
        assert_eq!(realpath(&root, "/usr/lib/rel.so").as_deref(), Some("/lib/ld-musl-aarch64.so.1"));
        assert_eq!(realpath(&root, "/usr/../lib/./ld-musl-aarch64.so.1").as_deref(), Some("/lib/ld-musl-aarch64.so.1"));
        assert_eq!(realpath(&root, "/nope"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
