//! wasm-env: runs WebAssembly programs built for the wasm-env Linux ABI
//! (docs/abi.md) on any host.
//!
//!   wasm-env run [--root DIR] [--env KEY=VALUE]... [--wayland[=headless|=host]] [--trace] PROGRAM [ARGS]...
//!   wasm-env convert [--lazy] [--no-precompile] IMAGE DIR   (see convert.rs)
//!   wasm-env shell [--wayland] DIR [SHELL-ARGS]...   a login shell in an environment
//!   wasm-env export DIR OUT[.tar]   an environment as an OCI image (see oci.rs)
//!
//! PROGRAM is a path inside the environment (e.g. /bin/sh), or else a
//! .wasm file on the host.
//!
//! --root   directory the program sees as / (default: the current directory)
//! --env    environment variable for the program (repeatable; default: a
//!          minimal PATH/HOME/TERM, not the host environment)
//! --trace  log every syscall to stderr (also WASM_ENV_TRACE=1)
//! --wayland  a Wayland compositor for the environment, the runtime's own:
//!          graphical programs' windows are windows of the host's, on any
//!          host (headless if it has no window system). =headless: no
//!          windows (tests); =host: the host's own compositor instead
//!          (Linux hosts running Wayland)

// (wgpu's types nest deeply enough for the Send and Sync checks of the
// threads that hold them.)
#![recursion_limit = "256"]

mod abi;
mod convert;
mod events;
mod fork;
mod fs;
mod gpu;
mod image;
mod inotify;
mod kernel;
mod locks;
mod memory;
mod modules;
mod net;
mod oci;
mod pipe;
mod process;
mod shared;
mod syscall;
mod terminal;
mod thread;
mod translate;
mod unix;
mod wayland;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::kernel::Kernel;
use crate::process::{ExecImage, Inherit};

fn usage() -> ! {
    eprintln!(
        "usage: wasm-env run [--root DIR] [--env KEY=VALUE]... [--argv0 NAME] [--wayland[=headless|=host]] [--trace] PROGRAM [ARGS]...\n\
         \x20      wasm-env convert [--lazy] [--no-precompile] IMAGE DIR\n\
         \x20      wasm-env shell [--wayland] DIR [SHELL-ARGS]...\n\
         \x20      wasm-env export DIR OUT[.tar]"
    );
    std::process::exit(2);
}

/// `wasm-env convert`: see convert.rs.
fn convert(args: impl Iterator<Item = String>) -> Result<()> {
    let mut opts = convert::Options { translate: true, precompile: true };
    let mut positional = Vec::new();
    for a in args {
        match a.as_str() {
            "--lazy" => opts.translate = false,
            "--no-precompile" => opts.precompile = false,
            s if s.starts_with("--") => bail!("unknown option {s}"),
            _ => positional.push(a),
        }
    }
    let [image, dir] = <[String; 2]>::try_from(positional).unwrap_or_else(|_| usage());
    convert::convert(&image, std::path::Path::new(&dir), &opts)
}

/// `wasm-env shell DIR [ARGS]...`: a login shell in an environment (bash if
/// it has one), with the variables a terminal session has, as `run` args.
fn shell_args(mut args: impl Iterator<Item = String>) -> Vec<String> {
    let mut dir = args.next().unwrap_or_else(|| usage());
    let wayland = dir.starts_with("--wayland").then(|| dir.clone());
    if wayland.is_some() {
        dir = args.next().unwrap_or_else(|| usage());
    }
    let shell = if PathBuf::from(&dir).join("bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm".into());
    let mut out: Vec<String> = vec!["--root".into(), dir];
    if let Some(w) = wayland {
        out.push(w);
    }
    for var in [
        format!("TERM={term}"),
        "HOME=/root".into(),
        "USER=root".into(),
        "LOGNAME=root".into(),
        format!("SHELL={shell}"),
        "LANG=C.UTF-8".into(),
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
    ] {
        out.extend(["--env".into(), var]);
    }
    out.extend(["--argv0".into(), format!("-{}", shell.rsplit('/').next().unwrap())]);
    out.push(shell.into());
    out.extend(args);
    out
}

/// How graphical programs are served (`--wayland`).
enum Wayland {
    Off,
    /// The runtime's compositor, with host windows (if the host has a
    /// window system) or none.
    Builtin { windows: bool },
    /// The host's own compositor, at this socket.
    Host(PathBuf),
}

/// The environment's XDG_RUNTIME_DIR with `--wayland` (programs run as root).
const GUEST_RUNTIME_DIR: &str = "/run/user/0";

/// The host's Wayland compositor socket: $WAYLAND_DISPLAY, a path or a name
/// in $XDG_RUNTIME_DIR.
fn host_wayland() -> Result<PathBuf> {
    let display = std::env::var_os("WAYLAND_DISPLAY").context("--wayland: WAYLAND_DISPLAY isn't set (no Wayland session)")?;
    let path = PathBuf::from(&display);
    let path = if path.is_absolute() {
        path
    } else {
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("--wayland: XDG_RUNTIME_DIR isn't set")?).join(path)
    };
    if !path.exists() {
        bail!("--wayland: no compositor socket at {}", path.display());
    }
    Ok(path)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut args: Box<dyn Iterator<Item = String>> = match args.next().as_deref() {
        Some("run") => Box::new(args),
        Some("convert") => return convert(args),
        Some("export") => {
            let (Some(dir), Some(out), None) = (args.next(), args.next(), args.next()) else { usage() };
            return oci::export(std::path::Path::new(&dir), std::path::Path::new(&out));
        }
        Some("shell") => Box::new(shell_args(args).into_iter()),
        _ => usage(),
    };
    let mut root = std::env::current_dir()?;
    let mut env = vec![];
    let mut trace = std::env::var_os("WASM_ENV_TRACE").is_some_and(|v| v != "0");
    let mut wayland = Wayland::Off;
    // The program's argv[0] (default: its path, or a host file's name).
    let mut argv0: Option<String> = None;
    let program = loop {
        match args.next() {
            Some(a) if a == "--root" => root = PathBuf::from(args.next().unwrap_or_else(|| usage())),
            Some(a) if a == "--env" => env.push(args.next().unwrap_or_else(|| usage())),
            Some(a) if a == "--trace" => trace = true,
            Some(a) if a == "--wayland" => wayland = Wayland::Builtin { windows: true },
            Some(a) if a == "--wayland=headless" => wayland = Wayland::Builtin { windows: false },
            Some(a) if a == "--wayland=host" => wayland = Wayland::Host(host_wayland()?),
            Some(a) if a == "--argv0" => argv0 = Some(args.next().unwrap_or_else(|| usage())),
            Some(a) if a.starts_with("--") => bail!("unknown option {a}"),
            Some(a) => break a,
            None => usage(),
        }
    };
    if env.is_empty() {
        env = ["PATH=/usr/local/bin:/usr/bin:/bin", "HOME=/root", "TERM=xterm"].map(String::from).to_vec();
    }
    // The compositor, reached from the environment as
    // $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY.
    let graphical = !matches!(wayland, Wayland::Off);
    if graphical {
        for var in [format!("XDG_RUNTIME_DIR={GUEST_RUNTIME_DIR}"), "WAYLAND_DISPLAY=wayland-0".into()] {
            let name = var.split('=').next().unwrap().to_string() + "=";
            if !env.iter().any(|e| e.starts_with(&name)) {
                env.push(var);
            }
        }
    }
    let env: Vec<Vec<u8>> = env.into_iter().map(String::into_bytes).collect();
    let root = root.canonicalize().with_context(|| format!("root directory {}", root.display()))?;
    if graphical {
        let dir = root.join(GUEST_RUNTIME_DIR.trim_start_matches('/'));
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let rest: Vec<String> = args.collect();

    // A path inside the environment, or a .wasm file on the host.
    let fs = fs::Fs::new(root.clone());
    let in_root = program.starts_with('/')
        && fs.resolve("/", &program, true).is_ok_and(|g| fs.host(&g).is_file());
    let image = if in_root {
        let argv = std::iter::once(argv0.clone().unwrap_or_else(|| program.clone())).chain(rest).map(String::into_bytes).collect();
        process::resolve_exec(&fs, &program, argv, env, trace)
            .map_err(|e| anyhow::anyhow!("{program}: can't execute (errno {e})"))?
    } else {
        // A host file: a .wasm module, or an arm64 ELF to translate.
        let mut host = PathBuf::from(&program);
        let mut interp = None;
        let mut head = Vec::new();
        if let Ok(f) = std::fs::File::open(&host) {
            let _ = std::io::Read::read_to_end(&mut std::io::Read::take(f, 256), &mut head);
        }
        if translate::is_arm64_elf(&head) {
            let name = host.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            interp = process::interpreter(&fs, &host);
            host = translate::ensure_translated(&host, &root, &name, trace)
                .map_err(|e| anyhow::anyhow!("{program}: can't translate (errno {e})"))?;
        }
        let name = argv0.clone().unwrap_or_else(|| PathBuf::from(&program).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        let argv = std::iter::once(name).chain(rest).map(String::into_bytes).collect();
        ExecImage { host, path: program, argv, env, interp }
    };

    let kernel = Kernel::new(process::engine()?, root, trace);
    // The compositor. With host windows, the host's event loop needs this
    // (the main) thread; the program runs on another, as always.
    let mut windows = None;
    let mut compositor = None;
    match wayland {
        Wayland::Off => {}
        Wayland::Host(socket) => kernel.forward(&format!("{GUEST_RUNTIME_DIR}/wayland-0"), socket),
        Wayland::Builtin { windows: want } => {
            let display: std::sync::Arc<dyn wayland::Display> = match want.then(wayland::window::event_loop).flatten() {
                Some((windowed, run)) => {
                    windows = Some((windowed.clone(), run));
                    windowed
                }
                None => {
                    if want {
                        eprintln!("wasm-env: no window system on this host: the compositor is headless");
                    }
                    std::sync::Arc::new(wayland::Headless::default())
                }
            };
            compositor = Some(wayland::Compositor::start(&kernel.unix, display));
            // Its socket's file, as a compositor's would be.
            let _ = std::fs::File::create(kernel.root.join(wayland::SOCKET.trim_start_matches('/')));
        }
    }
    let pid = kernel.create(0);
    let terminal = terminal::Terminal::take(kernel.clone());
    // wasmtime runs guest code on the host stack, so give it room.
    let first = std::thread::Builder::new()
        .name(format!("pid {pid}"))
        .stack_size(8 << 20)
        .spawn({
            let kernel = kernel.clone();
            move || kernel.run(pid, image, Inherit::initial())
        })?;
    let status = match (windows, compositor) {
        (Some((windowed, run)), Some(comp)) => {
            // The event loop until the program ends.
            std::thread::spawn(move || {
                let status = first.join().map_or(134, |how| how.shell_status());
                windowed.exit(status);
            });
            run(comp)
        }
        (_, comp) => {
            let status = first.join().map_err(|_| anyhow::anyhow!("process thread panicked"))?.shell_status();
            drop(comp);
            status
        }
    };
    drop(terminal);
    std::process::exit(status);
}
