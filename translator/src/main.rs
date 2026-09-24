//! wasm-env-translate: translates arm64 Linux ELF executables into
//! WebAssembly programs for the wasm-env runtime (docs/abi.md).
//!
//!   wasm-env-translate census BINARY          count instructions by operation
//!   wasm-env-translate translate BINARY OUT   translate to OUT.wasm

mod elf;

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

fn census(path: &str) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    let image = elf::Image::load(&bytes)?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut undecodable = 0;
    for (_, _, insn) in image.decode_all() {
        match insn {
            Some(i) => *counts.entry(i.op().mnem().to_string()).or_default() += 1,
            None => undecodable += 1,
        }
    }
    let total: usize = counts.values().sum();
    let mut by_count: Vec<_> = counts.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1));
    println!("{total} instructions, {} distinct operations, {undecodable} undecodable words", by_count.len());
    for (op, n) in by_count {
        println!("{n:8} {op}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["census", path] => census(path),
        _ => bail!("usage: wasm-env-translate census BINARY | translate BINARY OUT.wasm"),
    }
}
