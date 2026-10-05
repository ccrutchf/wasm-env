//! `wasm-env convert IMAGE DIR`: turns an arm64 Linux image into an
//! environment wasm-env runs (PLAN.md, M5).
//!
//! IMAGE is an OCI image reference (`ubuntu:24.04`, `docker.io/library/
//! debian:12`, `ghcr.io/owner/name@sha256:…`), pulled from its registry for
//! linux/arm64, or a local root filesystem (a directory, or a tarball). The
//! layers are unpacked into DIR in order (honouring whiteouts), so the
//! image's files, including its package database, are as the image has
//! them. Then every arm64 program and shared library in it is translated
//! into the image's own translation cache (`/var/cache/wasm-env`), so the
//! environment starts without translating and carries its translations with
//! it; anything added later (a `pip install`) translates on first use.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use sha2::Digest;

pub struct Options {
    /// Translate the image's programs and libraries now (else on first use).
    pub translate: bool,
    /// Also compile the translations for this host (wasmtime's code cache).
    pub precompile: bool,
}

pub fn convert(image: &str, out: &Path, opts: &Options) -> Result<()> {
    let local = Path::new(image);
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    if crate::oci::is_layout(local) {
        crate::oci::import(local, out)?;
    } else if local.is_dir() {
        copy_tree(local, out)?;
    } else if local.is_file() {
        eprintln!("unpacking {image}");
        let file = std::fs::File::open(local)?;
        let tarball: Box<dyn Read> =
            if image.ends_with(".gz") || image.ends_with(".tgz") { Box::new(flate2::read::GzDecoder::new(file)) } else { Box::new(file) };
        unpack_layer(tarball, out)?;
    } else {
        pull(image, out)?;
    }
    std::fs::write(out.join(".wasm-env-image"), format!("{image}\n"))?;
    // The image keeps its own translations, made now or on first use.
    std::fs::create_dir_all(out.join("var/cache/wasm-env/translated"))?;
    install_guest_files(out)?;
    if opts.translate {
        translate_all(out, opts.precompile)?;
    }
    eprintln!("converted {image} into {}; run it with: wasm-env shell {}", out.display(), out.display());
    Ok(())
}

/// Files of wasm-env's own that environments get: its OpenCL driver
/// (packages/opencl, built by flake.nix into `lib/wasm-env/guest` beside
/// the program; in the source tree, by packages/opencl/build.sh), and its
/// registration with the Khronos loader.
pub fn install_guest_files(root: &Path) -> Result<()> {
    let installed = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("../lib/wasm-env/guest")));
    let source_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build/packages/opencl");
    let Some(dir) = [installed, Some(source_tree)].into_iter().flatten().find(|d| d.join("libwasm-env-opencl.so").exists()) else {
        eprintln!("note: wasm-env's OpenCL driver isn't built (packages/opencl/build.sh); the environment won't have OpenCL");
        return Ok(());
    };
    let lib = root.join("usr/lib/wasm-env");
    std::fs::create_dir_all(&lib)?;
    std::fs::copy(dir.join("libwasm-env-opencl.so"), lib.join("libwasm-env-opencl.so"))?;
    let vendors = root.join("etc/OpenCL/vendors");
    std::fs::create_dir_all(&vendors)?;
    std::fs::write(vendors.join("wasm-env.icd"), "/usr/lib/wasm-env/libwasm-env-opencl.so\n")?;
    Ok(())
}

/// Copies a directory tree (a root filesystem given as a directory),
/// keeping symlinks as they are.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() || crate::fs::links::is_link(&src, &entry.metadata()?) {
            let _ = std::fs::remove_file(&dst);
            let target = crate::fs::links::read(&src)?;
            crate::fs::links::create(&target.to_string_lossy(), &dst)?;
        } else if kind.is_dir() {
            std::fs::create_dir_all(&dst)?;
            copy_tree(&src, &dst)?;
        } else if kind.is_file() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

// --- Pulling from a registry ---------------------------------------------------------

/// An image reference: registry host, repository, tag or digest.
#[derive(Debug, PartialEq)]
struct Reference {
    registry: String,
    repository: String,
    reference: String,
}

fn parse_reference(s: &str) -> Reference {
    let (name, reference) = match s.split_once('@') {
        Some((n, d)) => (n, d.to_string()),
        None => match s.rsplit_once(':') {
            Some((n, t)) if !t.contains('/') => (n, t.to_string()),
            _ => (s, "latest".to_string()),
        },
    };
    let (registry, repository) = match name.split_once('/') {
        Some((host, rest)) if host.contains('.') || host.contains(':') || host == "localhost" => {
            (host.to_string(), rest.to_string())
        }
        _ => ("docker.io".to_string(), name.to_string()),
    };
    let (registry, repository) = if registry == "docker.io" {
        let repo = if repository.contains('/') { repository } else { format!("library/{repository}") };
        ("registry-1.docker.io".to_string(), repo)
    } else {
        (registry, repository)
    };
    Reference { registry, repository, reference }
}

struct Registry {
    agent: ureq::Agent,
    reference: Reference,
    token: Option<String>,
}

const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, \
    application/vnd.docker.distribution.manifest.list.v2+json, \
    application/vnd.oci.image.manifest.v1+json, \
    application/vnd.docker.distribution.manifest.v2+json";

impl Registry {
    fn new(reference: Reference) -> Registry {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        Registry { agent, reference, token: None }
    }

    /// GETs `path` (under /v2/<repository>/), authenticating as the
    /// registry asks (anonymous bearer tokens, as for public images).
    fn get(&mut self, path: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>> {
        self.get_from(path, accept, 0)
    }

    /// `get` from byte `from` on (a Range request, to resume a download; the
    /// response is 206 if the server honoured it).
    fn get_from(&mut self, path: &str, accept: &str, from: u64) -> Result<ureq::http::Response<ureq::Body>> {
        let url = format!("https://{}/v2/{}/{path}", self.reference.registry, self.reference.repository);
        for attempt in 0..2 {
            let mut req = self.agent.get(&url).header("Accept", accept);
            if let Some(t) = &self.token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            if from > 0 {
                req = req.header("Range", format!("bytes={from}-"));
            }
            let resp = req.call().with_context(|| format!("fetching {url}"))?;
            // (Unauthorized: no token yet, or it expired during a long pull.)
            if resp.status() == 401 && attempt == 0 {
                let challenge = resp.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                self.token = Some(self.authenticate(&challenge)?);
                continue;
            }
            if !resp.status().is_success() {
                bail!("{url}: HTTP {}", resp.status());
            }
            return Ok(resp);
        }
        bail!("{url}: not authorized")
    }

    /// An anonymous token for the challenge `Bearer realm="…",service="…"`.
    fn authenticate(&self, challenge: &str) -> Result<String> {
        let params: Vec<(String, String)> = challenge
            .trim_start_matches("Bearer ")
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
            .collect();
        let param = |k: &str| params.iter().find(|p| p.0 == k).map(|p| p.1.clone());
        let realm = param("realm").context("the registry asked for authentication without a realm")?;
        let scope = format!("repository:{}:pull", self.reference.repository);
        let mut req = self.agent.get(&realm).query("scope", &scope);
        if let Some(service) = param("service") {
            req = req.query("service", &service);
        }
        let mut resp = req.call()?;
        if !resp.status().is_success() {
            bail!("{realm}: HTTP {}", resp.status());
        }
        let body: serde_json::Value = serde_json::from_str(&resp.body_mut().read_to_string()?)?;
        body.get("token")
            .or_else(|| body.get("access_token"))
            .and_then(|t| t.as_str())
            .map(String::from)
            .context("no token in the registry's answer")
    }

    fn json(&mut self, path: &str, accept: &str) -> Result<serde_json::Value> {
        let mut resp = self.get(path, accept)?;
        Ok(serde_json::from_str(&resp.body_mut().read_to_string()?)?)
    }
}

/// Where downloaded layers are kept (by digest), so converting again
/// doesn't download them again.
fn blob_cache() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("wasm-env/blobs")
}

/// A layer being downloaded: what has arrived so far.
struct Download {
    file: std::fs::File,
    hasher: sha2::Sha256,
    written: u64,
}

impl Download {
    /// Fetches the rest of blob `digest` (all of it, if the server won't
    /// resume: then it starts again).
    fn fetch(&mut self, registry: &mut Registry, digest: &str) -> Result<()> {
        use std::io::Seek;
        let resp = registry.get_from(&format!("blobs/{digest}"), "*/*", self.written)?;
        if self.written > 0 && resp.status() != 206 {
            self.file.set_len(0)?;
            self.file.rewind()?;
            self.hasher = sha2::Sha256::new();
            self.written = 0;
        }
        let mut reader = resp.into_body().into_reader();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            self.hasher.update(&buf[..n]);
            self.file.write_all(&buf[..n])?;
            self.written += n as u64;
        }
    }
}

fn pull(image: &str, out: &Path) -> Result<()> {
    let reference = parse_reference(image);
    let tag = reference.reference.clone();
    let mut registry = Registry::new(reference);
    eprintln!("pulling {image} (linux/arm64)");
    let mut manifest = registry.json(&format!("manifests/{tag}"), MANIFEST_TYPES)?;
    // An index lists a manifest per platform: take linux/arm64.
    if let Some(list) = manifest.get("manifests").and_then(|m| m.as_array()) {
        let arm64 = list
            .iter()
            .find(|m| {
                let p = &m["platform"];
                p["os"] == "linux" && p["architecture"] == "arm64"
            })
            .context("the image has no linux/arm64 variant")?;
        let digest = arm64["digest"].as_str().context("a manifest without a digest")?.to_string();
        manifest = registry.json(&format!("manifests/{digest}"), MANIFEST_TYPES)?;
    }
    let layers = manifest["layers"].as_array().context("a manifest without layers")?.clone();
    let cache = blob_cache();
    std::fs::create_dir_all(&cache)?;
    for (k, layer) in layers.iter().enumerate() {
        let digest = layer["digest"].as_str().context("a layer without a digest")?;
        let media = layer["mediaType"].as_str().unwrap_or("");
        let size = layer["size"].as_u64().unwrap_or(0);
        let hex = digest.strip_prefix("sha256:").context("only sha256 digests are supported")?;
        let blob = cache.join(hex);
        if !blob.exists() {
            eprintln!("  layer {}/{}: {digest} ({:.1} MB)", k + 1, layers.len(), size as f64 / 1e6);
            let tmp = blob.with_extension("part");
            let mut download = Download { file: std::fs::File::create(&tmp)?, hasher: sha2::Sha256::new(), written: 0 };
            // A dropped connection (large layers, slow links) resumes where it
            // stopped, a few times, waiting longer each time.
            const TRIES: u32 = 5;
            for attempt in 1..=TRIES {
                match download.fetch(&mut registry, digest) {
                    Ok(()) => break,
                    Err(e) if attempt < TRIES => {
                        eprintln!("  layer {}/{}: {e:#}; resuming at {:.1} MB ({attempt}/{})", k + 1, layers.len(), download.written as f64 / 1e6, TRIES - 1);
                        std::thread::sleep(std::time::Duration::from_secs(1 << attempt));
                    }
                    Err(e) => return Err(e),
                }
            }
            let got = format!("{:x}", download.hasher.finalize());
            if got != hex {
                let _ = std::fs::remove_file(&tmp);
                bail!("layer {digest}: downloaded data has digest sha256:{got}");
            }
            std::fs::rename(&tmp, &blob)?;
        }
        apply_layer(&blob, media, out)?;
    }
    Ok(())
}

/// Applies a layer blob (a tar, possibly gzipped, as its media type says).
pub fn apply_layer(blob: &Path, media: &str, out: &Path) -> Result<()> {
    let file = std::fs::File::open(blob)?;
    let reader: Box<dyn Read> = if media.ends_with("gzip") || media.ends_with("tar.gz") {
        Box::new(flate2::read::GzDecoder::new(file))
    } else if media.ends_with(".tar") || media.ends_with("tar") {
        Box::new(file)
    } else {
        bail!("{}: unsupported layer media type {media}", blob.display());
    };
    unpack_layer(reader, out)
}

/// Applies one layer (a tar stream) to `out`: whiteouts (`.wh.NAME`
/// removes NAME from the layers below, `.wh..wh..opq` empties the
/// directory) and then its files. Device nodes are skipped (the runtime
/// provides /dev), and ownership isn't kept (everything is the user's).
fn unpack_layer(reader: impl Read, out: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let rel: PathBuf = path.components().filter(|c| matches!(c, std::path::Component::Normal(_))).collect();
        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name == ".wh..wh..opq" {
            let dir = out.join(rel.parent().unwrap_or(Path::new("")));
            // Remove what lower layers put there (this layer's entries come
            // from the tar, which lists the marker first by convention).
            if let Ok(children) = std::fs::read_dir(&dir) {
                for c in children.flatten() {
                    let p = c.path();
                    let _ = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                }
            }
            continue;
        }
        if let Some(target) = name.strip_prefix(".wh.") {
            let p = out.join(rel.parent().unwrap_or(Path::new(""))).join(target);
            let _ = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
            continue;
        }
        let kind = entry.header().entry_type();
        if kind.is_block_special() || kind.is_character_special() || kind.is_fifo() {
            continue;
        }
        // Replacing something that isn't a directory: remove it first (it
        // may be read-only, or a symlink the new file mustn't follow).
        let dest = out.join(&rel);
        if !kind.is_dir() && dest.symlink_metadata().is_ok_and(|m| !m.is_dir()) {
            let _ = std::fs::remove_file(&dest);
        }
        // Symlinks as the environment keeps them on this host (fs::links).
        if kind == tar::EntryType::Symlink {
            let target = entry.link_name()?.map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
            let dest = out.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::fs::links::create(&target, &dest).with_context(|| format!("unpacking {}", rel.display()))?;
            continue;
        }
        entry.unpack_in(out).with_context(|| format!("unpacking {}", rel.display()))?;
    }
    Ok(())
}

// --- Translating ---------------------------------------------------------------------

/// What an ELF file in the image is to translate as.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Program,
    Library,
}

/// Classifies an arm64 ELF file: shared objects (by name or because
/// nothing runs them as a program, like the dynamic linker) are libraries;
/// executables (ET_EXEC, or position-independent with a PT_INTERP) are
/// programs.
fn classify(path: &Path, head: &[u8]) -> Option<Kind> {
    if !crate::translate::is_arm64_elf(head) {
        return None;
    }
    let e_type = u16::from_le_bytes([head[16], head[17]]);
    let name = path.file_name()?.to_string_lossy();
    match e_type {
        2 => Some(Kind::Program),
        3 if name.contains(".so") => Some(Kind::Library),
        3 if crate::process::has_interpreter(head) => Some(Kind::Program),
        3 => Some(Kind::Library),
        _ => None,
    }
}

/// Every arm64 program and library in the image.
fn find_elf(root: &Path) -> Vec<(PathBuf, Kind)> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    let skip = ["proc", "sys", "dev", "var/cache/wasm-env"].map(|d| root.join(d));
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if !skip.contains(&p) {
                    dirs.push(p);
                }
            } else if t.is_file() && e.metadata().is_ok_and(|m| m.len() > 64) {
                let mut head = vec![0u8; 4096];
                let n = std::fs::File::open(&p).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
                head.truncate(n);
                if let Some(kind) = classify(&p, &head) {
                    found.push((p, kind));
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Translates every program and library in the image into its cache, a
/// few at a time (each translation compiles in parallel too).
fn translate_all(root: &Path, precompile: bool) -> Result<()> {
    let cache = root.join("var/cache/wasm-env/translated");
    std::fs::create_dir_all(&cache)?;
    let mut files = find_elf(root);
    // Libraries first: they get their guest ranges in a stable order.
    files.sort_by_key(|(p, k)| (*k == Kind::Program, p.clone()));
    let total = files.len();
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let workers = cores.div_ceil(2).clamp(1, 8);
    eprintln!("translating {total} programs and libraries ({workers} at a time)");
    if std::env::var_os("WASM_ENV_JOBS").is_none() {
        // SAFETY: before any thread of ours starts.
        unsafe { std::env::set_var("WASM_ENV_JOBS", (cores / workers).max(1).to_string()) };
    }
    let engine = if precompile { Some(crate::process::engine()?) } else { None };
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let failed = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                while let Some((path, kind)) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let result = (|| -> Result<PathBuf> {
                        let bytes = std::fs::read(path)?;
                        let wasm = match kind {
                            Kind::Library => {
                                let path = std::fs::canonicalize(path)?;
                                crate::translate::ensure_library(&path, root, false).map(|(w, _, _)| w)
                            }
                            Kind::Program => crate::translate::ensure_program(path, &bytes, root, false),
                        }
                        .map_err(|e| anyhow::anyhow!("translation failed (errno {e})"))?;
                        if let Some(engine) = &engine {
                            crate::process::load_module(engine, &wasm)?;
                        }
                        Ok(wasm)
                    })();
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    let shown = path.strip_prefix(root).unwrap_or(path).display().to_string();
                    match result {
                        Ok(_) => eprintln!("  [{n}/{total}] /{shown}"),
                        Err(e) => {
                            eprintln!("  [{n}/{total}] /{shown}: {e:#}");
                            failed.lock().unwrap().push(shown);
                        }
                    }
                }
            });
        }
    });
    let failed = failed.into_inner().unwrap();
    if !failed.is_empty() {
        eprintln!("{} of {total} didn't translate (they'll be tried again when used)", failed.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references() {
        let r = |registry: &str, repository: &str, reference: &str| Reference {
            registry: registry.into(),
            repository: repository.into(),
            reference: reference.into(),
        };
        assert_eq!(parse_reference("ubuntu:24.04"), r("registry-1.docker.io", "library/ubuntu", "24.04"));
        assert_eq!(parse_reference("ubuntu"), r("registry-1.docker.io", "library/ubuntu", "latest"));
        assert_eq!(parse_reference("docker.io/pytorch/pytorch:2.4"), r("registry-1.docker.io", "pytorch/pytorch", "2.4"));
        assert_eq!(parse_reference("ghcr.io/o/n@sha256:abc"), r("ghcr.io", "o/n", "sha256:abc"));
        assert_eq!(parse_reference("localhost:5000/img"), r("localhost:5000", "img", "latest"));
    }

    #[test]
    fn layers_apply_whiteouts() {
        let dir = std::env::temp_dir().join(format!("wasm-env-layer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let layer = |entries: &[(&str, &[u8])]| {
            let mut b = tar::Builder::new(Vec::new());
            for (path, data) in entries {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, path, *data).unwrap();
            }
            b.into_inner().unwrap()
        };
        unpack_layer(&layer(&[("a/keep", b"1"), ("a/gone", b"2"), ("b/old", b"3")])[..], &dir).unwrap();
        unpack_layer(&layer(&[("a/.wh.gone", b""), ("b/.wh..wh..opq", b""), ("b/new", b"4"), ("a/keep", b"5")])[..], &dir).unwrap();
        assert_eq!(std::fs::read(dir.join("a/keep")).unwrap(), b"5", "overwritten");
        assert!(!dir.join("a/gone").exists(), "whited out");
        assert!(!dir.join("b/old").exists(), "opaque directory");
        assert_eq!(std::fs::read(dir.join("b/new")).unwrap(), b"4");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classifies_elf_files() {
        let mut head = vec![0u8; 64];
        head[..4].copy_from_slice(b"\x7fELF");
        head[4] = 2;
        head[5] = 1;
        head[18] = 183;
        head[16] = 2; // ET_EXEC
        assert_eq!(classify(Path::new("/bin/ls"), &head), Some(Kind::Program));
        head[16] = 3; // ET_DYN
        assert_eq!(classify(Path::new("/lib/libc.so.6"), &head), Some(Kind::Library));
        assert_eq!(classify(Path::new("/lib/ld-linux-aarch64.so.1"), &head), Some(Kind::Library));
        head[18] = 62; // x86-64
        assert_eq!(classify(Path::new("/bin/ls"), &head), None);
    }
}
