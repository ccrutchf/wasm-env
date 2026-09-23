//! M-1 benchmark harness: wasm32 vs memory64, on wasmtime (Cranelift) and
//! wasm2c + clang (LLVM), against a native baseline.
//!
//! Usage: bench-harness [--runs N] [--build DIR] [--out FILE] [--core N] [BENCH...]
//!
//! Every run gets a fresh instance: setup() is untimed, run() is timed.
//! Executable configurations (native and wasm2c builds from bench/Makefile)
//! follow the same protocol and print "<ns> <checksum>" per run.
//!
//! To reduce noise the harness pins itself (and so its child processes) to
//! one CPU core, and interleaves configurations: each round runs every
//! configuration once, starting from a different one each round.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use wasmtime::{Config, Engine, Instance, Module, Store};

const BENCHES: &[&str] = &["list", "tree", "stream", "sort", "hash", "interp", "sha256"];

/// A wasm build plus the wasmtime configuration used to run it.
struct WasmConfig {
    name: &'static str,
    dir: &'static str,
    /// Force explicit bounds checks by disabling the virtual-memory reservation.
    explicit_bounds_checks: bool,
    /// Keep Cranelift's Spectre mitigation on explicit bounds checks (the default).
    spectre: bool,
    /// Mask 64-bit indexes to this many bits instead of bounds-checking them
    /// (requires the patched wasmtime-internal-cranelift in vendor/).
    mask_bits: Option<u32>,
}

const WASM_CONFIGS: &[WasmConfig] = &[
    WasmConfig { name: "wasm32", dir: "wasm32", explicit_bounds_checks: false, spectre: true, mask_bits: None },
    WasmConfig { name: "wasm32-bounds", dir: "wasm32", explicit_bounds_checks: true, spectre: true, mask_bits: None },
    WasmConfig { name: "wasm64", dir: "wasm64", explicit_bounds_checks: false, spectre: true, mask_bits: None },
    WasmConfig { name: "wasm64-nospectre", dir: "wasm64", explicit_bounds_checks: false, spectre: false, mask_bits: None },
    WasmConfig { name: "wasm64-masked", dir: "wasm64", explicit_bounds_checks: false, spectre: true, mask_bits: Some(38) },
];

/// Executable configurations: bench/build/<name>/<bench>.
const EXE_CONFIGS: &[&str] = &["native", "w2c-wasm32", "w2c-wasm64", "w2c-wasm64-masked"];

/// Something that can perform one timed run of one benchmark.
enum Runner {
    Exe(PathBuf),
    Wasm { engine: Engine, module: Module },
}

impl Runner {
    /// Returns (nanoseconds, checksum) for one run.
    fn run_once(&self) -> Result<(u64, u64)> {
        match self {
            Runner::Exe(exe) => {
                let out = Command::new(exe)
                    .arg("1")
                    .output()
                    .with_context(|| format!("running {}", exe.display()))?;
                if !out.status.success() {
                    bail!("{} exited with {}", exe.display(), out.status);
                }
                let stdout = String::from_utf8(out.stdout)?;
                let (ns, sum) = stdout.trim().split_once(' ').context("malformed output")?;
                Ok((ns.parse()?, sum.parse()?))
            }
            Runner::Wasm { engine, module } => {
                let mut store = Store::new(engine, ());
                let instance = Instance::new(&mut store, module, &[])?;
                let setup = instance.get_typed_func::<(), ()>(&mut store, "setup")?;
                let run = instance.get_typed_func::<(), i64>(&mut store, "run")?;
                setup.call(&mut store, ())?;
                let t = Instant::now();
                let sum = run.call(&mut store, ())? as u64;
                Ok((t.elapsed().as_nanos() as u64, sum))
            }
        }
    }
}

#[derive(Serialize)]
struct Measurement {
    bench: String,
    config: String,
    median_ns: u64,
    min_ns: u64,
    /// Interquartile range as a fraction of the median.
    spread: f64,
    times_ns: Vec<u64>,
    checksum: u64,
    /// Cranelift compile time (wasmtime configurations only).
    compile_ns: Option<u64>,
}

#[derive(Serialize)]
struct Report {
    host_os: &'static str,
    host_arch: &'static str,
    runs: usize,
    pinned_core: Option<usize>,
    load_average_start: Option<f64>,
    load_average_end: Option<f64>,
    measurements: Vec<Measurement>,
}

struct Args {
    runs: usize,
    build: PathBuf,
    out: Option<PathBuf>,
    core: Option<usize>,
    benches: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args { runs: 9, build: "bench/build".into(), out: None, core: None, benches: vec![] };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--runs" => args.runs = it.next().context("--runs needs a value")?.parse()?,
            "--build" => args.build = it.next().context("--build needs a value")?.into(),
            "--out" => args.out = Some(it.next().context("--out needs a value")?.into()),
            "--core" => args.core = Some(it.next().context("--core needs a value")?.parse()?),
            _ if a.starts_with("--") => bail!("unknown flag {a}"),
            _ => args.benches.push(a),
        }
    }
    if args.benches.is_empty() {
        args.benches = BENCHES.iter().map(|s| s.to_string()).collect();
    }
    Ok(args)
}

fn engine_for(cfg: &WasmConfig) -> Result<Engine> {
    let mut config = Config::new();
    config.wasm_memory64(true);
    config.cranelift_opt_level(wasmtime::OptLevel::Speed);
    if cfg.explicit_bounds_checks {
        config.memory_reservation(0);
        config.memory_guard_size(0);
    }
    if !cfg.spectre {
        // SAFETY: only affects generated code's speculation hardening.
        unsafe { config.cranelift_flag_set("enable_heap_access_spectre_mitigation", "false") };
    }
    if let Some(bits) = cfg.mask_bits {
        // A fixed reservation of 2^bits bytes that never moves, so every
        // masked index lands inside it.
        config.memory_reservation(1 << bits);
        config.memory_may_move(false);
        config.memory_guard_size(64 << 20);
    }
    Ok(Engine::new(&config)?)
}

/// Compiles a module for `cfg`. Returns the runner and the compile time.
fn compile(cfg: &WasmConfig, wasm: &Path) -> Result<(Runner, Duration)> {
    let engine = engine_for(cfg)?;
    let bytes = std::fs::read(wasm).with_context(|| format!("reading {}", wasm.display()))?;
    // The patched compiler reads the mask width from the environment.
    // SAFETY: nothing else in the harness reads or writes the environment
    // concurrently.
    match cfg.mask_bits {
        Some(bits) => unsafe { std::env::set_var("WASM_ENV_MASK_BITS", bits.to_string()) },
        None => unsafe { std::env::remove_var("WASM_ENV_MASK_BITS") },
    }
    let t = Instant::now();
    let module = Module::new(&engine, &bytes)?;
    Ok((Runner::Wasm { engine, module }, t.elapsed()))
}

/// Pins this thread (and child processes, which inherit it) to one core.
/// Defaults to the last core, which tends to see fewer interrupts.
fn pin(core: Option<usize>) -> Option<usize> {
    let cores = core_affinity::get_core_ids()?;
    let id = match core {
        Some(n) => *cores.iter().find(|c| c.id == n)?,
        None => *cores.last()?,
    };
    core_affinity::set_for_current(id).then_some(id.id)
}

/// One-minute load average (Linux only).
fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn ms(ns: u64) -> String {
    format!("{:.1}", Duration::from_nanos(ns).as_secs_f64() * 1e3)
}

fn geomean(xs: &[f64]) -> f64 {
    (xs.iter().map(|x| x.ln()).sum::<f64>() / xs.len() as f64).exp()
}

fn main() -> Result<()> {
    let args = parse_args()?;

    let pinned = pin(args.core);
    match pinned {
        Some(c) => println!("pinned to core {c}"),
        None => println!("warning: could not pin to a core (unsupported on this OS?)"),
    }
    let load_start = load_average();
    if let Some(l) = load_start.filter(|l| *l > 1.0) {
        println!("warning: load average is {l:.2}; results will be noisy");
    }

    let names: Vec<&str> =
        EXE_CONFIGS.iter().copied().chain(WASM_CONFIGS.iter().map(|c| c.name)).collect();
    println!("{} runs per configuration; times are median ms (interquartile range)\n", args.runs);
    print!("{:<8}", "bench");
    for n in &names {
        print!(" {n:>20}");
    }
    println!();

    let mut measurements = vec![];
    // ratios[i] collects column i's time relative to native (column 0).
    let mut ratios = vec![vec![]; names.len()];

    for bench in &args.benches {
        // Prepare every configuration up front, so rounds only do timed work.
        let mut runners = vec![];
        let mut compile_times = vec![];
        for exe in EXE_CONFIGS {
            let path = args.build.join(exe).join(bench);
            if !path.exists() {
                bail!("missing {} (run `make -C bench`)", path.display());
            }
            runners.push(Runner::Exe(path));
            compile_times.push(None);
        }
        for cfg in WASM_CONFIGS {
            let (runner, t) = compile(cfg, &args.build.join(cfg.dir).join(format!("{bench}.wasm")))?;
            runners.push(runner);
            compile_times.push(Some(t.as_nanos() as u64));
        }

        // Interleave: each round runs every configuration once, rotating the
        // starting configuration so none is always first or last.
        let n = runners.len();
        let mut times = vec![vec![]; n];
        let mut checksums = vec![None; n];
        for round in 0..args.runs {
            for k in 0..n {
                let i = (round + k) % n;
                let (ns, sum) = runners[i].run_once()?;
                if checksums[i].is_some_and(|c| c != sum) {
                    bail!("{bench}/{}: checksum changed between runs", names[i]);
                }
                checksums[i] = Some(sum);
                times[i].push(ns);
            }
        }

        let native_median = {
            let mut t = times[0].clone();
            t.sort_unstable();
            percentile(&t, 0.5)
        };
        print!("{bench:<8}");
        for i in 0..n {
            let mut sorted = times[i].clone();
            sorted.sort_unstable();
            let median = percentile(&sorted, 0.5);
            let spread =
                (percentile(&sorted, 0.75) - percentile(&sorted, 0.25)) as f64 / median as f64;
            ratios[i].push(median as f64 / native_median as f64);
            print!(" {:>10} ({:>4.1}%)  ", ms(median), spread * 100.0);

            // The C code does the same work on every target, so checksums must agree.
            if checksums[i] != checksums[0] {
                eprintln!(
                    "\nwarning: {bench}: checksum mismatch ({} = {:?}, native = {:?})",
                    names[i], checksums[i], checksums[0]
                );
            }
            measurements.push(Measurement {
                bench: bench.clone(),
                config: names[i].into(),
                median_ns: median,
                min_ns: sorted[0],
                spread,
                times_ns: times[i].clone(),
                checksum: checksums[i].unwrap_or_default(),
                compile_ns: compile_times[i],
            });
        }
        println!();
    }

    println!("\ngeomean relative to native:");
    for (n, r) in names.iter().zip(&ratios) {
        println!("  {n:<20} {:.3}x", geomean(r));
    }
    let load_end = load_average();
    if let (Some(s), Some(e)) = (load_start, load_end) {
        println!("\nload average: {s:.2} at start, {e:.2} at end");
    }

    if let Some(out) = args.out {
        let report = Report {
            host_os: std::env::consts::OS,
            host_arch: std::env::consts::ARCH,
            runs: args.runs,
            pinned_core: pinned,
            load_average_start: load_start,
            load_average_end: load_end,
            measurements,
        };
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&out, serde_json::to_string_pretty(&report)?)?;
        println!("wrote {}", out.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks the masked-memory semantics claimed in the patched
    /// bounds_checks.rs: in-bounds loads work, out-of-bounds loads below
    /// 2^bits still trap, and loads at 2^bits and above wrap.
    #[test]
    fn masked_memory64_semantics() -> Result<()> {
        let cfg = WASM_CONFIGS.iter().find(|c| c.name == "wasm64-masked").unwrap();
        let bits = cfg.mask_bits.unwrap();
        let engine = engine_for(cfg)?;
        unsafe { std::env::set_var("WASM_ENV_MASK_BITS", bits.to_string()) };
        let module = Module::new(
            &engine,
            r#"(module
                 (memory i64 1)
                 (data (i64.const 8) "\2a")
                 (func (export "load") (param i64) (result i32)
                   local.get 0 i32.load8_u))"#,
        )?;
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[])?;
        let load = instance.get_typed_func::<i64, i32>(&mut store, "load")?;

        assert_eq!(load.call(&mut store, 8)?, 42, "in-bounds load");
        assert!(load.call(&mut store, 65536).is_err(), "just past the end traps");
        assert!(load.call(&mut store, 1 << 30).is_err(), "far past the end traps");
        assert_eq!(load.call(&mut store, (1 << bits) + 8)?, 42, "2^bits + 8 wraps to 8");
        assert_eq!(load.call(&mut store, -1i64 << bits | 8)?, 42, "high bits are ignored");
        Ok(())
    }
}
