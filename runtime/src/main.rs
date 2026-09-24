//! wasm-env: runs WebAssembly programs built for the wasm-env Linux ABI
//! (docs/abi.md) on any host.
//!
//!   wasm-env run [--root DIR] [--env KEY=VALUE]... [--trace] PROGRAM.wasm [ARGS]...
//!
//! --root   directory the program sees as / (default: the current directory)
//! --env    environment variable for the program (repeatable; default: a
//!          minimal PATH/HOME/TERM, not the host environment)
//! --trace  log every syscall to stderr (also WASM_ENV_TRACE=1)

mod abi;
mod fs;
mod memory;
mod process;
mod syscall;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

fn usage() -> ! {
    eprintln!("usage: wasm-env run [--root DIR] [--env KEY=VALUE]... [--trace] PROGRAM.wasm [ARGS]...");
    std::process::exit(2);
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("run") {
        usage();
    }
    let mut root = std::env::current_dir()?;
    let mut env = vec![];
    let mut trace = std::env::var_os("WASM_ENV_TRACE").is_some_and(|v| v != "0");
    let wasm = loop {
        match args.next() {
            Some(a) if a == "--root" => root = PathBuf::from(args.next().unwrap_or_else(|| usage())),
            Some(a) if a == "--env" => env.push(args.next().unwrap_or_else(|| usage())),
            Some(a) if a == "--trace" => trace = true,
            Some(a) if a.starts_with("--") => bail!("unknown option {a}"),
            Some(a) => break PathBuf::from(a),
            None => usage(),
        }
    };
    if env.is_empty() {
        env = ["PATH=/usr/local/bin:/usr/bin:/bin", "HOME=/root", "TERM=xterm"].map(String::from).to_vec();
    }
    let root = root.canonicalize().with_context(|| format!("root directory {}", root.display()))?;
    let name = wasm.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let argv = std::iter::once(name).chain(args).collect();

    let engine = process::engine()?;
    let status = process::run(&engine, process::Program { wasm: &wasm, root, argv, env, trace })?;
    std::process::exit(status);
}
