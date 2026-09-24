//! wasm-env-translate: translates arm64 Linux ELF executables into
//! WebAssembly programs for the wasm-env runtime (docs/abi.md).
//!
//!   wasm-env-translate census BINARY   count instructions by operation
//!   wasm-env-translate blocks BINARY   list the basic blocks discovery finds
//!   wasm-env-translate translate BINARY OUT.wasm [--root DIR] [--dlopen PATH]...
//!                                 [--lib SO]... [--interp LD_SO] [--deps-out FILE]   (needs `lift`)
//!
//! With `--root`, the dynamic linker and libraries are found inside DIR the
//! way the dynamic linker will find them; `--dlopen` adds libraries the
//! program loads later. `--lib`/`--interp` name host files directly.

#[cfg(feature = "lift")]
mod deps;
mod discover;
mod elf;
#[cfg(feature = "lift")]
mod lift;
#[cfg_attr(not(feature = "lift"), allow(dead_code))]
mod link;
#[cfg(test)]
mod testutil;

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

/// Counts instructions by operation, most frequent first, plus the number
/// of words that don't decode.
fn census_of(image: &elf::Image) -> (Vec<(String, usize)>, usize) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut undecodable = 0;
    for (_, _, insn) in image.decode_all() {
        match insn {
            Some(i) => *counts.entry(i.op().mnem().to_string()).or_default() += 1,
            None => undecodable += 1,
        }
    }
    let mut by_count: Vec<_> = counts.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    (by_count, undecodable)
}

fn census(path: &str) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    let image = elf::Image::load(&bytes)?;
    let (ops, undecodable) = census_of(&image);
    let total: usize = ops.iter().map(|o| o.1).sum();
    println!("{total} instructions, {} distinct operations, {undecodable} undecodable words", ops.len());
    for (op, n) in ops {
        println!("{n:8} {op}");
    }
    Ok(())
}

/// What to translate along with the executable.
#[cfg(feature = "lift")]
#[derive(Default)]
struct Inputs {
    /// Where to list the host files the translation read (`--deps-out`), so a
    /// cache can tell when it goes stale.
    deps_out: Option<String>,
    /// Shared libraries on the host (`--lib`).
    libs: Vec<String>,
    /// The dynamic linker on the host (`--interp`).
    interp: Option<String>,
    /// A root filesystem to find the interpreter and libraries in (`--root`).
    root: Option<String>,
    /// Libraries (paths inside the root, or directories of them) the program
    /// `dlopen`s (`--dlopen`); their dependencies are included too.
    dlopen: Vec<String>,
}

/// Translates an arm64 executable (with any shared libraries) into a
/// wasm-env program.
#[cfg(feature = "lift")]
fn translate(input: &str, output: &str, inputs: &Inputs) -> Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("reading {input}"))?;
    // (name, bytes): the name is the path the dynamic linker will open.
    let read_files = std::cell::RefCell::new(Vec::new());
    let read = |name: &str, host: &std::path::Path| -> Result<(String, Vec<u8>)> {
        read_files.borrow_mut().push(host.to_path_buf());
        Ok((name.to_string(), std::fs::read(host).with_context(|| format!("reading {}", host.display()))?))
    };
    let mut libs = Vec::new();
    let mut interp = inputs.interp.as_deref().map(|p| read(p, p.as_ref())).transpose()?;
    if let Some(root) = &inputs.root {
        let root = std::path::Path::new(root);
        let found = deps::resolve(root, &bytes, &inputs.dlopen)?;
        if interp.is_none() {
            interp = found.interp.map(|d| read(&d.guest, &d.host)).transpose()?;
        }
        for d in found.libs {
            libs.push(read(&d.guest, &d.host)?);
        }
    }
    for l in &inputs.libs {
        libs.push(read(l, l.as_ref())?);
    }
    let image = if libs.is_empty() && interp.is_none() {
        elf::Image::load(&bytes)?
    } else {
        elf::Image::load_dynamic(&bytes, &libs, interp.as_ref())?
    };
    if let Some(path) = &inputs.deps_out {
        let list: String = read_files.borrow().iter().map(|p| format!("{}\n", p.display())).collect();
        std::fs::write(path, list).with_context(|| format!("writing {path}"))?;
    }
    let prog = discover::discover(&image);
    let lifted = lift::lift(&image, &prog);
    for (op, (n, example)) in &lifted.unsupported {
        eprintln!("warning: {n} × `{op}` not supported yet (traps if executed): {example}");
    }
    let tc = link::Toolchain::from_env().context("$WASM_CC isn't set (run inside `nix develop`)")?;
    let image_end = image.segments.iter().map(|s| s.vaddr + s.memsz).max().unwrap_or(0);
    let work = std::env::temp_dir().join(format!("wasm-env-translate-{}", std::process::id()));
    let wasm = link::link_chunked(&tc, &lifted.data, &lifted.chunks, image_end, &work);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::write(output, wasm?).with_context(|| format!("writing {output}"))?;
    eprintln!("{input}: {} blocks in {} module(s) → {output}", lifted.blocks, lifted.chunks.len());
    Ok(())
}

/// Reports the blocks discovery found.
fn blocks(path: &str) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    let image = elf::Image::load(&bytes)?;
    let prog = discover::discover(&image);
    println!("{} instructions decoded, {} blocks", prog.insns.len(), prog.leaders.len());
    for a in &prog.leaders {
        match image.function_at(*a) {
            Some(f) => println!("{a:#x} {f}"),
            None => println!("{a:#x}"),
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["census", path] => census(path),
        ["blocks", path] => blocks(path),
        #[cfg(feature = "lift")]
        ["translate", input, output, libs @ ..] => {
            // translate INPUT OUT [--lib SO]... [--interp LD_SO] [--root DIR] [--dlopen PATH]...
            let mut inputs = Inputs::default();
            let mut it = libs.iter();
            while let Some(a) = it.next() {
                let mut value = || it.next().map(|s| s.to_string()).with_context(|| format!("{a} needs a value"));
                match *a {
                    "--lib" => inputs.libs.push(value()?),
                    "--interp" => inputs.interp = Some(value()?),
                    "--root" => inputs.root = Some(value()?),
                    "--dlopen" => inputs.dlopen.push(value()?),
                    "--deps-out" => inputs.deps_out = Some(value()?),
                    other => bail!("unexpected argument {other}"),
                }
            }
            translate(input, output, &inputs)
        }
        #[cfg(feature = "lift")]
        ["emit", input] => {
            let bytes = std::fs::read(input)?;
            let image = elf::Image::load(&bytes)?;
            let prog = discover::discover(&image);
            print!("{}", lift::lift(&image, &prog).ir());
            Ok(())
        }
        _ => bail!("usage: wasm-env-translate census|blocks BINARY | translate BINARY OUT.wasm"),
    }
}
