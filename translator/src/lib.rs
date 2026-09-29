//! wasm-env-translate as a library: the translator's parts, for its binary
//! (main.rs) and for the runtime, which translates code a program generates
//! at run time in its own process (see `translate_generated`).

#[cfg(feature = "lift")]
pub mod deps;
pub mod discover;
pub mod ehframe;
pub mod elf;
#[cfg(feature = "lift")]
pub mod lift;
#[cfg_attr(not(feature = "lift"), allow(dead_code))]
pub mod link;
#[cfg(test)]
mod testutil;
#[cfg(feature = "lift")]
pub mod wasmgen;

/// The pages holding a program's blocks: from the first block's page to
/// the end of the page where the last block ends.
#[cfg(feature = "lift")]
pub fn code_extent(prog: &discover::Program) -> (u64, u64) {
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

/// Translates code a program generated at run time: `bytes`, the mapping at
/// `base`, entered at `roots`; the module (as `translate --raw` makes it,
/// covering the pages its blocks are on). Direct compilation only (see
/// wasmgen): an error for what that doesn't compile, which the caller then
/// translates the other way (the translator's process, with clang).
#[cfg(feature = "lift")]
pub fn translate_generated(bytes: &[u8], base: u64, roots: &[u64]) -> Result<Vec<u8>, String> {
    let timings = std::env::var_os("WASM_ENV_TIMINGS").is_some();
    let t = std::time::Instant::now();
    let mut image = elf::Image::raw(bytes, "generated", base);
    image.roots.extend(roots);
    let prog = discover::discover(&image);
    let discovered = t.elapsed();
    let mut chunks = Vec::new();
    lift::lift(&image, &prog, true, None, &mut |c| chunks.push(c));
    let lifted = t.elapsed();
    let wasm = wasmgen::compile(&chunks)?;
    if timings {
        eprintln!(
            "[timing] generated code: {} blocks; discover {:.4} s, lift {:.4} s, compile {:.4} s",
            prog.leaders.len(),
            discovered.as_secs_f64(),
            (lifted - discovered).as_secs_f64(),
            (t.elapsed() - lifted).as_secs_f64()
        );
    }
    let (lo, hi) = code_extent(&prog);
    link::add_library_info(&wasm, lo, hi).map_err(|e| e.to_string())
}
