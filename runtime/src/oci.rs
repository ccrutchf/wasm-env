//! Environments as OCI images (PLAN.md, "Distribution").
//!
//! `wasm-env export DIR OUT` writes a converted environment (see
//! convert.rs) as an OCI image layout (a directory, or a tar of one when
//! OUT ends in `.tar`), for platform wasi/wasm: one layer holding the whole
//! root filesystem, translations included (they're in the image's
//! `/var/cache/wasm-env`, keyed by content and the translator's version), so
//! the image runs wherever it's unpacked without translating again.
//! `wasm-env convert` reads such a layout back (or any OCI layout, such as
//! `docker save` writes), preferring its wasi/wasm image, else linux/arm64.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::Digest;

const LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";

/// A writer that hashes what passes through it.
struct Hashing<W: Write> {
    inner: W,
    hasher: sha2::Sha256,
    len: u64,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Hashing<W> {
    fn new(inner: W) -> Self {
        Hashing { inner, hasher: sha2::Sha256::new(), len: 0 }
    }
    fn finish(self) -> (W, String, u64) {
        (self.inner, format!("sha256:{:x}", self.hasher.finalize()), self.len)
    }
}

/// Writes `bytes` as a blob of the layout at `layout`; returns its digest.
fn put_blob(layout: &Path, bytes: &[u8]) -> Result<String> {
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(bytes));
    std::fs::write(layout.join("blobs/sha256").join(&digest[7..]), bytes)?;
    Ok(digest)
}

/// Adds the tree under `dir` to a tar, with paths relative to it, keeping
/// symlinks as links.
fn append_tree<W: Write>(tar: &mut tar::Builder<W>, root: &Path, dir: &Path) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let rel = path.strip_prefix(root)?;
        let kind = e.file_type()?;
        if kind.is_dir() {
            tar.append_dir(rel, &path)?;
            append_tree(tar, root, &path)?;
        } else if !kind.is_symlink() && crate::fs::links::is_link(&path, &e.metadata()?) {
            // A link file (Windows hosts: fs::links): archived as the symlink.
            let target = crate::fs::links::read(&path)?;
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_mode(0o777);
            header.set_size(0);
            tar.append_link(&mut header, rel, &target).with_context(|| format!("archiving {}", path.display()))?;
        } else if kind.is_file() || kind.is_symlink() {
            tar.append_path_with_name(&path, rel).with_context(|| format!("archiving {}", path.display()))?;
        }
    }
    Ok(())
}

/// `wasm-env export`: writes the environment in `dir` as an OCI image.
pub fn export(dir: &Path, out: &Path) -> Result<()> {
    let as_tar = out.extension().is_some_and(|e| e == "tar");
    let layout = if as_tar { out.with_extension("layout.tmp") } else { out.to_path_buf() };
    let _ = std::fs::remove_dir_all(&layout);
    std::fs::create_dir_all(layout.join("blobs/sha256"))?;
    std::fs::write(layout.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#)?;

    // The layer: the whole root filesystem, gzipped. Its digest is of the
    // compressed bytes; the config's diff_id of the tar itself.
    eprintln!("archiving {}", dir.display());
    let tmp = layout.join("blobs/layer.tmp");
    let compressed = Hashing::new(std::fs::File::create(&tmp)?);
    let gz = flate2::write::GzEncoder::new(compressed, flate2::Compression::fast());
    let mut tar = tar::Builder::new(Hashing::new(gz));
    tar.follow_symlinks(false);
    append_tree(&mut tar, dir, dir)?;
    let (gz, diff_id, _) = tar.into_inner()?.finish();
    let (file, layer_digest, layer_size) = gz.finish()?.finish();
    file.sync_all()?;
    std::fs::rename(&tmp, layout.join("blobs/sha256").join(&layer_digest[7..]))?;

    let source = std::fs::read_to_string(dir.join(".wasm-env-image")).unwrap_or_default().trim().to_string();
    let config = serde_json::json!({
        "architecture": "wasm",
        "os": "wasi",
        "config": {
            "Cmd": ["/bin/bash", "-l"],
            "Env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin", "HOME=/root", "LANG=C.UTF-8"],
            "Labels": { "run.wasm-env.converted-from": source },
        },
        "rootfs": { "type": "layers", "diff_ids": [diff_id] },
    });
    let config = serde_json::to_vec(&config)?;
    let config_digest = put_blob(&layout, &config)?;
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST,
        "config": { "mediaType": CONFIG, "digest": config_digest, "size": config.len() },
        "layers": [{ "mediaType": LAYER, "digest": layer_digest, "size": layer_size }],
    });
    let manifest = serde_json::to_vec(&manifest)?;
    let manifest_digest = put_blob(&layout, &manifest)?;
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": MANIFEST,
            "digest": manifest_digest,
            "size": manifest.len(),
            "platform": { "architecture": "wasm", "os": "wasi" },
        }],
    });
    std::fs::write(layout.join("index.json"), serde_json::to_vec_pretty(&index)?)?;

    if as_tar {
        let mut tar = tar::Builder::new(std::fs::File::create(out)?);
        // oci-layout first, so readers can tell what the tar is early.
        tar.append_path_with_name(layout.join("oci-layout"), "oci-layout")?;
        tar.append_path_with_name(layout.join("index.json"), "index.json")?;
        tar.append_dir_all("blobs", layout.join("blobs"))?;
        tar.finish()?;
        std::fs::remove_dir_all(&layout)?;
    }
    eprintln!("exported {} as {} ({:.1} MB layer)", dir.display(), out.display(), layer_size as f64 / 1e6);
    Ok(())
}

/// Whether `path` is an OCI image layout: a directory with an `oci-layout`
/// file, or a tar with one at its top (within its first entries).
pub fn is_layout(path: &Path) -> bool {
    if path.is_dir() {
        return path.join("oci-layout").is_file();
    }
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut tar = tar::Archive::new(file);
    let Ok(entries) = tar.entries() else { return false };
    entries.take(16).flatten().any(|e| e.path().is_ok_and(|p| p == Path::new("oci-layout")))
}

/// Unpacks an OCI image layout's image into `out`: its wasi/wasm image if
/// it has one, else its linux/arm64 one (each layer checked by digest).
pub fn import(path: &Path, out: &Path) -> Result<()> {
    let (layout, unpacked) = if path.is_dir() {
        (path.to_path_buf(), None)
    } else {
        let dir = out.with_extension("oci.tmp");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        tar::Archive::new(std::fs::File::open(path)?).unpack(&dir)?;
        (dir.clone(), Some(dir))
    };
    let result = import_layout(&layout, out);
    if let Some(dir) = unpacked {
        let _ = std::fs::remove_dir_all(dir);
    }
    result
}

fn blob(layout: &Path, digest: &str) -> Result<PathBuf> {
    let hex = digest.strip_prefix("sha256:").context("only sha256 digests are supported")?;
    Ok(layout.join("blobs/sha256").join(hex))
}

fn json_blob(layout: &Path, digest: &str) -> Result<serde_json::Value> {
    Ok(serde_json::from_slice(&std::fs::read(blob(layout, digest)?)?)?)
}

fn import_layout(layout: &Path, out: &Path) -> Result<()> {
    let index: serde_json::Value = serde_json::from_slice(&std::fs::read(layout.join("index.json"))?)?;
    let pick = |list: &[serde_json::Value]| -> Option<serde_json::Value> {
        let on = |arch: &str, os: &str| list.iter().find(|m| m["platform"]["architecture"] == arch && m["platform"]["os"] == os);
        on("wasm", "wasi").or_else(|| on("arm64", "linux")).or(list.first()).cloned()
    };
    let mut entry = pick(index["manifests"].as_array().context("index.json lists no images")?).context("no images")?;
    // An index may point at another index.
    let manifest = loop {
        let digest = entry["digest"].as_str().context("an entry without a digest")?;
        let v = json_blob(layout, digest)?;
        match v["manifests"].as_array() {
            Some(list) => entry = pick(list).context("an empty index")?,
            None => break v,
        }
    };
    for layer in manifest["layers"].as_array().context("a manifest without layers")? {
        let digest = layer["digest"].as_str().context("a layer without a digest")?;
        let path = blob(layout, digest)?;
        let mut hasher = sha2::Sha256::new();
        let mut file = std::fs::File::open(&path)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        if format!("sha256:{:x}", hasher.finalize()) != digest {
            bail!("layer {digest} doesn't match its digest");
        }
        crate::convert::apply_layer(&path, layer["mediaType"].as_str().unwrap_or(LAYER), out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_and_import_round_trip() {
        let base = std::env::temp_dir().join(format!("wasm-env-oci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (dir, back) = (base.join("env"), base.join("back"));
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        std::fs::write(dir.join("etc/hostname"), b"wasm-env\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("hostname", dir.join("etc/link")).unwrap();
        let tar = base.join("image.tar");
        export(&dir, &tar).unwrap();
        assert!(is_layout(&tar));
        std::fs::create_dir_all(&back).unwrap();
        import(&tar, &back).unwrap();
        assert_eq!(std::fs::read(back.join("etc/hostname")).unwrap(), b"wasm-env\n");
        #[cfg(unix)]
        assert_eq!(std::fs::read_link(back.join("etc/link")).unwrap(), Path::new("hostname"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
