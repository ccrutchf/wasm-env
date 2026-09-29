//! wasm-env-translate: translates arm64 Linux ELF executables into
//! WebAssembly programs for the wasm-env runtime (docs/abi.md).
//!
//!   wasm-env-translate census BINARY   count instructions by operation
//!   wasm-env-translate blocks BINARY   list the basic blocks discovery finds
//!   wasm-env-translate translate BINARY OUT.wasm [--root DIR] [--dlopen PATH]...
//!                                 [--lib SO]... [--interp LD_SO] [--deps-out FILE] [--roots FILE]
//!                                 [--layout FILE]   (needs `lift`)
//!
//! With `--root`, the dynamic linker and libraries are found inside DIR the
//! way the dynamic linker will find them; `--dlopen` adds libraries the
//! program loads later. `--lib`/`--interp` name host files directly.

#[cfg(feature = "lift")]
mod deps;
mod discover;
mod ehframe;
mod elf;
#[cfg(feature = "lift")]
mod lift;
#[cfg(feature = "lift")]
mod wasmgen;
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
    /// Extra block roots (`--roots FILE`): "object<TAB>offset" lines, as the
    /// runtime records addresses a previous run found untranslated.
    roots: Option<String>,
    /// Where to list the host files the translation read (`--deps-out`), so a
    /// cache can tell when it goes stale.
    deps_out: Option<String>,
    /// Library bases from an earlier translation (`--layout FILE`,
    /// "base<TAB>name" lines), reused where they still fit so added
    /// libraries don't move the others; the file is then rewritten.
    layout: Option<String>,
    /// Shared libraries on the host (`--lib`).
    libs: Vec<String>,
    /// The dynamic linker on the host (`--interp`).
    interp: Option<String>,
    /// A root filesystem to find the interpreter and libraries in (`--root`).
    root: Option<String>,
    /// Libraries (paths inside the root, or directories of them) the program
    /// `dlopen`s (`--dlopen`); their dependencies are included too.
    dlopen: Vec<String>,
    /// Translate just this object (`--object`), for a program whose
    /// libraries are modules of their own (docs/abi.md, "Libraries"): the
    /// executable, or with `--base` a shared library placed there.
    object: bool,
    base: Option<u64>,
    /// The input is code generated at run time (`--raw`), at `base`.
    raw: bool,
    /// Only this page's region (`--page`): a patch the runtime makes when
    /// a run reaches a block the translation missed there.
    page: Option<u64>,
    /// Where to write the blocks discovery found (`--leaders-out`), and, with
    /// `--page`, where to read them (`--leaders`) instead of discovering the
    /// whole program again.
    leaders_out: Option<String>,
    leaders: Option<String>,
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
        // The program's own path inside the root, for $ORIGIN in its RPATH.
        let exe_guest = std::fs::canonicalize(input).ok().zip(std::fs::canonicalize(root).ok()).and_then(|(i, r)| {
            i.strip_prefix(&r).ok().map(|rel| format!("/{}", rel.display()))
        });
        let found = deps::resolve(root, &bytes, exe_guest.as_deref(), &inputs.dlopen)?;
        if interp.is_none() {
            interp = found.interp.map(|d| read(&d.guest, &d.host)).transpose()?;
        }
        read_files.borrow_mut().extend(found.scanned);
        for d in found.libs {
            libs.push(read(&d.guest, &d.host)?);
        }
    }
    for l in &inputs.libs {
        libs.push(read(l, l.as_ref())?);
    }
    let library = inputs.base.is_some() || inputs.page.is_some();
    let mut image = if inputs.raw {
        elf::Image::raw(&bytes, input, inputs.base.context("--raw needs --base")?)
    } else if inputs.object {
        elf::Image::load_object(&bytes, input, inputs.base)?
    } else if libs.is_empty() && interp.is_none() {
        elf::Image::load(&bytes)?
    } else {
        let mut layout = std::collections::HashMap::new();
        if let Some(text) = inputs.layout.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text.lines() {
                if let Some((base, name)) = line.split_once('\t')
                    && let Ok(base) = u64::from_str_radix(base.trim_start_matches("0x"), 16)
                {
                    layout.insert(name.to_string(), base);
                }
            }
        }
        let image = elf::Image::load_dynamic(&bytes, &libs, interp.as_ref(), &layout)?;
        if let Some(path) = &inputs.layout {
            let text: String = image.objects[1..].iter().map(|(name, base, _)| format!("{base:#x}\t{name}\n")).collect();
            std::fs::write(path, text).with_context(|| format!("writing {path}"))?;
        }
        image
    };
    image.objects[0].0 = input.to_string();
    if let Some(path) = &inputs.roots {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        for line in text.lines() {
            let Some((object, offset)) = line.split_once('\t') else { continue };
            let Ok(offset) = u64::from_str_radix(offset.trim().trim_start_matches("0x"), 16) else { continue };
            if let Some((_, base, _)) = image.objects.iter().find(|o| o.0 == object) {
                image.roots.push(base + offset);
                // (Generated code is entered where the JIT says; its roots
                // aren't missed targets of code discovery read poorly.)
                if !inputs.raw {
                    image.missed.push(base + offset);
                }
            }
        }
    }
    if let Some(path) = &inputs.deps_out {
        let list: String = read_files.borrow().iter().map(|p| format!("{}\n", p.display())).collect();
        std::fs::write(path, list).with_context(|| format!("writing {path}"))?;
    }
    // WASM_ENV_TIMINGS=1 reports how long each phase takes.
    let timings = std::env::var_os("WASM_ENV_TIMINGS").is_some();
    let phase = std::cell::Cell::new(std::time::Instant::now());
    let lap = |what: &str| {
        if timings {
            let rss = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).map(|l| l.split_whitespace().nth(1).unwrap_or("0").to_string()))
                .unwrap_or_default();
            eprintln!("[timing] {what}: {:.2} s, {rss} kB resident", phase.get().elapsed().as_secs_f64());
        }
        phase.set(std::time::Instant::now());
    };
    lap("load");
    let prog = match (inputs.page, &inputs.leaders) {
        (Some(page), Some(path)) => {
            let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
            let known: Vec<u64> = bytes.as_chunks::<8>().0.iter().map(|w| u64::from_le_bytes(*w)).collect();
            discover::page_program(&image, page / 0x1000 * 0x1000, &known, &image.roots)
        }
        _ => discover::discover(&image),
    };
    lap("discover");
    if let Some(path) = &inputs.leaders_out {
        let bytes: Vec<u8> = prog.leaders.iter().flat_map(|a| a.to_le_bytes()).collect();
        std::fs::write(path, bytes).with_context(|| format!("writing {path}"))?;
    }
    let image_end = image.segments.iter().map(|s| s.vaddr + s.memsz).max().unwrap_or(0);
    let work = std::env::temp_dir().join(format!("wasm-env-translate-{}", std::process::id()));
    // Code the program waits for (generated code, a page translated again)
    // is compiled straight to WebAssembly if it can be (wasmgen), which
    // takes milliseconds; otherwise, and for everything else, with clang
    // and wasm-ld. Each code module goes to disk as soon as it's lifted (see
    // lift::lift), or for the direct compiler stays in memory (it's small).
    let direct = (inputs.raw || inputs.page.is_some()) && std::env::var_os("WASM_ENV_NO_WASMGEN").is_none();
    let mut chunks = Vec::new();
    let mut build = None;
    let mut failed = None;
    let make_build = || -> Result<link::Build> {
        let tc = link::Toolchain::from_env()
            .context("no toolchain found: install wasm-env with its toolchain (nix profile install), set $WASM_ENV_TOOLCHAIN, or run inside `nix develop`")?;
        let tc: &'static link::Toolchain = Box::leak(Box::new(tc));
        let mut b = link::Build::new(tc, &work, library)?;
        if inputs.raw {
            b.quick();
        }
        Ok(b)
    };
    if !direct {
        build = Some(make_build()?);
    }
    let lifted = lift::lift(&image, &prog, library, inputs.page, &mut |chunk| {
        if direct {
            chunks.push(chunk);
        } else if failed.is_none()
            && let Some(b) = build.as_mut()
            && let Err(e) = b.chunk(&chunk)
        {
            failed = Some(e);
        }
    });
    if let Some(e) = failed {
        return Err(e);
    }
    lap("lift");
    for (op, (n, example)) in &lifted.unsupported {
        eprintln!("warning: {n} × `{op}` not supported yet (traps if executed): {example}");
    }
    let compiled = match direct {
        true => match wasmgen::compile(&chunks) {
            Ok(wasm) => Ok(wasm),
            Err(why) => {
                // WASM_ENV_DEBUG_WASMGEN=1 says why clang compiles it.
                if std::env::var_os("WASM_ENV_DEBUG_WASMGEN").is_some() {
                    eprintln!("wasmgen: {why}");
                }
                let mut b = make_build()?;
                for c in &chunks {
                    b.chunk(c)?;
                }
                b.finish(&lifted.data, image_end)
            }
        },
        false => build.take().unwrap().finish(&lifted.data, image_end),
    };
    let wasm = compiled.and_then(|wasm| {
        if let Some(page) = inputs.page {
            let start = lift::region_of(page);
            return link::add_library_info(&wasm, start, start + lift::REGION_SPAN);
        }
        if inputs.raw {
            // Generated code: the module covers the whole pages its blocks
            // are in, not the whole mapping (see the runtime's
            // modules::translate_code).
            let (lo, hi) = code_extent(&prog);
            return link::add_library_info(&wasm, lo, hi);
        }
        if library {
            return link::add_library_info(&wasm, image.objects[0].1, image.objects[0].2);
        }
        let segments: Vec<(u64, &[u8])> = image.segments.iter().map(|s| (s.vaddr, s.data.as_slice())).collect();
        link::add_image(&wasm, &segments)
    });
    lap("compile and link");
    // WASM_ENV_KEEP_WORK=1 keeps the generated IR and objects for debugging.
    if std::env::var_os("WASM_ENV_KEEP_WORK").is_some() {
        eprintln!("kept {}", work.display());
        for (name, base, end) in &image.libs {
            eprintln!("  {base:#x}..{end:#x} {name}");
        }
    } else {
        let _ = std::fs::remove_dir_all(&work);
    }
    std::fs::write(output, wasm?).with_context(|| format!("writing {output}"))?;
    eprintln!("{input}: {} blocks in {} module(s) → {output}", lifted.blocks, lifted.chunks);
    Ok(())
}

/// The pages holding a program's blocks: from the first block's page to
/// the end of the page where the last block ends.
#[cfg(feature = "lift")]
fn code_extent(prog: &discover::Program) -> (u64, u64) {
    const PAGE: u64 = 0x1000;
    let (Some(&first), Some(&last)) = (prog.leaders.first(), prog.leaders.last()) else { return (0, 0) };
    let mut end = last;
    while let Some(i) = prog.insns.get(&end) {
        end += 4;
        if discover::is_control(&i) || prog.leaders.contains(&end) {
            break;
        }
    }
    (first / PAGE * PAGE, end.max(last + 4).next_multiple_of(PAGE))
}

/// Reports the blocks discovery found.
fn blocks(path: &str) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    let image = elf::Image::load(&bytes)?;
    let prog = discover::discover(&image);
    println!("{} instructions decoded, {} blocks", prog.insns.iter().count(), prog.leaders.len());
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
                    "--roots" => inputs.roots = Some(value()?),
                    "--layout" => inputs.layout = Some(value()?),
                    "--object" => inputs.object = true,
                    "--raw" => inputs.raw = true,
                    "--leaders-out" => inputs.leaders_out = Some(value()?),
                    "--leaders" => inputs.leaders = Some(value()?),
                    "--page" => {
                        let v = value()?;
                        inputs.page = Some(u64::from_str_radix(v.trim_start_matches("0x"), 16).with_context(|| format!("--page {v}"))?);
                    }
                    "--base" => {
                        let v = value()?;
                        let base = u64::from_str_radix(v.trim_start_matches("0x"), 16).with_context(|| format!("--base {v}"))?;
                        inputs.base = Some(base);
                    }
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
            let mut chunks = String::new();
            let lifted = lift::lift(&image, &prog, false, None, &mut |c| {
                chunks.push_str("\n; ---- chunk ----\n");
                chunks.push_str(&c);
            });
            print!("{}{chunks}", lifted.data);
            Ok(())
        }
        _ => bail!("usage: wasm-env-translate census|blocks BINARY | translate BINARY OUT.wasm"),
    }
}
