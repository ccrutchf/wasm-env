//! Turning lifted LLVM IR into a wasm-env program: compile the IR and the
//! support code (support/runtime.c) for wasm64 with clang, and link them
//! with wasm-ld.
//!
//! Memory layout: the guest image occupies its original addresses at the
//! bottom of memory, so `--global-base` puts everything wasm-ld places
//! (guest state, the support code's data, the stack) above it. The mmap
//! region the runtime manages starts at `__heap_base`, above that.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const SUPPORT_C: &str = include_str!("../support/runtime.c");

/// WebAssembly target and features, shared by the lifted code and the
/// support code.
const WASM_FLAGS: &[&str] =
    &["--target=wasm64-unknown-unknown", "-mtail-call", "-mbulk-memory", "-mexception-handling", "-mrelaxed-simd"];

/// Optimization level for the lifted IR. It is already low-level, and one
/// module can hold a whole translated library, so `-O2` makes clang use
/// enormous memory on a whole-library module (see PLAN.md, M3). Small and
/// medium programs compile fine at -O2; override with WASM_ENV_LIFT_OPT.
fn lift_opt() -> String {
    std::env::var("WASM_ENV_LIFT_OPT").unwrap_or_else(|_| "-O2".into())
}

pub struct Toolchain {
    pub cc: PathBuf,
    pub wasm_ld: PathBuf,
    /// compiler-rt builtins for wasm64 (`__multi3` and friends, which
    /// LLVM emits for 128-bit arithmetic and divide-by-constant). Built
    /// into the sysroot by toolchain/build.sh.
    pub builtins: Option<PathBuf>,
}

impl Toolchain {
    /// clang from $WASM_CC and wasm-ld from PATH (both in `nix develop`).
    pub fn from_env() -> Option<Toolchain> {
        let cc = PathBuf::from(std::env::var_os("WASM_CC")?);
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let builtins = repo.join("build/sysroot/lib/libclang_rt.builtins.a");
        Some(Toolchain { cc, wasm_ld: PathBuf::from("wasm-ld"), builtins: builtins.exists().then_some(builtins) })
    }
}

/// The lowest address wasm-ld may use: the end of the guest image, rounded
/// up to a WebAssembly page.
pub fn global_base(image_end: u64) -> u64 {
    image_end.max(1).next_multiple_of(65536)
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.output().with_context(|| format!("running {:?}", cmd.get_program()))?;
    if !out.status.success() {
        bail!("{:?} failed:\n{}", cmd.get_program(), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// How long one unit may take to compile before it's retried more simply
/// (see link_chunked): `$WASM_ENV_COMPILE_LIMIT` seconds, default 600.
fn compile_limit() -> std::time::Duration {
    let secs = std::env::var("WASM_ENV_COMPILE_LIMIT").ok().and_then(|s| s.parse().ok()).unwrap_or(600);
    std::time::Duration::from_secs(secs)
}

/// Runs `cmd`, giving up after `limit`: Ok(true) if it succeeded, Ok(false)
/// if it ran out of time (it's killed), an error if it failed. Its stderr
/// goes to a file, so a chatty command can't block on a full pipe.
fn run_limited(cmd: &mut Command, limit: std::time::Duration) -> Result<bool> {
    let log = std::env::temp_dir().join(format!(
        "wasm-env-cc-{}-{:?}.err",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut child = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(&log)?)
        .spawn()
        .with_context(|| format!("running {:?}", cmd.get_program()))?;
    let start = std::time::Instant::now();
    let result = loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                break Ok(true);
            }
            let err = std::fs::read_to_string(&log).unwrap_or_default();
            break Err(anyhow::anyhow!("{:?} failed:\n{err}", cmd.get_program()));
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break Ok(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let _ = std::fs::remove_file(&log);
    // A retry through `run` captures its output again, for error messages.
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    result
}

/// Compiles and links `ir` with the support code into a WebAssembly
/// module, using `work` for intermediate files.
#[cfg_attr(not(test), allow(dead_code))]
pub fn link(tc: &Toolchain, ir: &str, image_end: u64, work: &Path) -> Result<Vec<u8>> {
    std::fs::create_dir_all(work)?;
    let (ll, lifted_o) = (work.join("lifted.ll"), work.join("lifted.o"));
    let (support_c, support_o) = (work.join("support.c"), work.join("support.o"));
    let wasm = work.join("program.wasm");
    std::fs::write(&ll, ir)?;
    std::fs::write(&support_c, SUPPORT_C)?;
    run(Command::new(&tc.cc).args(WASM_FLAGS).arg(lift_opt()).arg("-c").arg(&ll).arg("-o").arg(&lifted_o))?;
    run(Command::new(&tc.cc).args(WASM_FLAGS).args(["-O2", "-ffreestanding", "-nostdlib", "-c"]).arg(&support_c).arg("-o").arg(&support_o))?;
    run(Command::new(&tc.wasm_ld)
        .args(["-m", "wasm64", "--no-entry", "-z", "stack-size=1048576"])
        .arg(format!("--global-base={}", global_base(image_end)))
        // Required by the wasm-env ABI (docs/abi.md), plus guest state for
        // debugging and tests.
        .args(["--export=__stack_pointer", "--export=__heap_base"])
        .args(["--export=R", "--export=F", "--export=V", "--export=TP"])
        .arg(&lifted_o)
        .arg(&support_o)
        .args(tc.builtins.iter())
        .arg("-o")
        .arg(&wasm))?;
    Ok(std::fs::read(&wasm)?)
}

/// Where compiled objects are cached: `$WASM_ENV_OBJECT_CACHE` (empty or "0"
/// disables it), else `~/.cache/wasm-env/objects`. Entries are named by content,
/// so they never go stale; nothing evicts them yet.
fn object_cache() -> Option<PathBuf> {
    match std::env::var_os("WASM_ENV_OBJECT_CACHE") {
        Some(d) if d.is_empty() || d == "0" => None,
        Some(d) => Some(PathBuf::from(d)),
        None => std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|d| d.join("wasm-env/objects")),
    }
}

/// A 128-bit content hash (two independent FNV-1a variants) of `parts`, in
/// hex. Parts are length-prefixed, so their boundaries count.
fn content_key(parts: &[&[u8]]) -> String {
    let (mut a, mut b) = (0xcbf2_9ce4_8422_2325u64, 0x6c62_272e_07bb_0142u64);
    let mut feed = |byte: u8| {
        a = (a ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        b = (b ^ byte as u64).wrapping_mul(0x0000_0001_0000_01b3).rotate_left(5);
    };
    for p in parts {
        for byte in (p.len() as u64).to_le_bytes().iter().chain(p.iter()) {
            feed(*byte);
        }
    }
    format!("{a:016x}{b:016x}")
}

/// Compiles a chunked program (a data module plus several code modules) and
/// the support code into one WebAssembly module. Each module compiles
/// separately, in parallel, so memory stays bounded no matter how large the
/// translated program is.
pub fn link_chunked(tc: &Toolchain, data: &str, chunks: &[String], image_end: u64, work: &Path) -> Result<Vec<u8>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    std::fs::create_dir_all(work)?;

    // The data module and support code compile like any other unit; the data
    // module is only declarations and tables, so it needs no optimization.
    let support_c = work.join("support.c");
    std::fs::write(&support_c, SUPPORT_C)?;
    let data_ll = work.join("data.ll");
    std::fs::write(&data_ll, data)?;

    // (source path, output object, opt level) for every unit. With an object
    // cache, the object's name is a hash of everything that determines it,
    // so an unchanged unit (most chunks, when retranslating) isn't rebuilt.
    let cache = object_cache();
    let mut units: Vec<(PathBuf, PathBuf, String)> = Vec::new();
    let mut unit = |name: &str, source: &[u8], opt: String| -> Result<()> {
        let src = work.join(name);
        std::fs::write(&src, source)?;
        let obj = match &cache {
            Some(dir) => dir.join(format!(
                "{}.o",
                content_key(&[source, opt.as_bytes(), WASM_FLAGS.join(" ").as_bytes(), tc.cc.as_os_str().as_encoded_bytes()])
            )),
            None => src.with_extension("o"),
        };
        units.push((src, obj, opt));
        Ok(())
    };
    unit("data.ll", data.as_bytes(), "-O0".into())?;
    unit("support.c", SUPPORT_C.as_bytes(), "-O2".into())?;
    for (i, chunk) in chunks.iter().enumerate() {
        unit(&format!("chunk{i}.ll"), chunk.as_bytes(), lift_opt())?;
    }
    if let Some(dir) = &cache {
        std::fs::create_dir_all(dir)?;
    }

    // Compile what isn't cached, in parallel across the available cores.
    let todo: Vec<&(PathBuf, PathBuf, String)> = units.iter().filter(|u| !u.1.exists()).collect();
    let next = AtomicUsize::new(0);
    let errors = std::sync::Mutex::new(Vec::<String>::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some((src, obj, opt)) = todo.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let is_c = src.extension().is_some_and(|e| e == "c");
                    let tmp = obj.with_extension(format!("{}.tmp", std::process::id()));
                    let mut cmd = Command::new(&tc.cc);
                    cmd.args(WASM_FLAGS).arg(opt);
                    if is_c {
                        cmd.args(["-ffreestanding", "-nostdlib"]);
                    }
                    cmd.arg("-c").arg(src).arg("-o").arg(&tmp);
                    // A unit that compiles pathologically slowly (LLVM's
                    // WebAssembly backend can all but hang on vectors its SLP
                    // vectorizer formed) is compiled again without that
                    // vectorizer: one unit's code gets slower, never the
                    // whole translation stuck.
                    let compiled = run_limited(&mut cmd, compile_limit()).and_then(|done| {
                        if !done {
                            eprintln!(
                                "warning: compiling {} took over {} s; again without the SLP vectorizer",
                                src.display(),
                                compile_limit().as_secs()
                            );
                            run(cmd.arg("-fno-slp-vectorize"))?;
                        }
                        Ok(())
                    });
                    // Compile to a temporary name, then rename: concurrent
                    // translations never see a partial object.
                    match compiled.and_then(|()| Ok(std::fs::rename(&tmp, obj)?)) {
                        Ok(()) => {}
                        Err(e) => {
                            let _ = std::fs::remove_file(&tmp);
                            errors.lock().unwrap().push(e.to_string());
                        }
                    }
                }
            });
        }
    });
    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        bail!("compiling modules failed:\n{}", errors.join("\n"));
    }

    if std::env::var_os("WASM_ENV_TIMINGS").is_some() {
        eprintln!("[timing] compiled {} of {} units", todo.len(), units.len());
    }
    let wasm = work.join("program.wasm");
    let mut ld = Command::new(&tc.wasm_ld);
    ld.args(["-m", "wasm64", "--no-entry", "-z", "stack-size=1048576"])
        .arg(format!("--global-base={}", global_base(image_end)))
        .args(["--export=__stack_pointer", "--export=__heap_base"])
        .args(["--export=R", "--export=F", "--export=V", "--export=TP"])
        // For the runtime's report when a translated program faults.
        .args(["--export=wasm_env_recent", "--export=wasm_env_recent_count"]);
    for (_, obj, _) in &units {
        ld.arg(obj);
    }
    ld.args(tc.builtins.iter()).arg("-o").arg(&wasm);
    run(&mut ld)?;
    Ok(std::fs::read(&wasm)?)
}

/// Adds active data segments to a linked wasm64 module: each (address,
/// bytes) is placed in memory at that address when the module is
/// instantiated. The guest image goes in this way, at its original
/// addresses, rather than being copied there at startup: wasmtime then maps
/// it copy-on-write from the compiled module, and a process only pays for
/// the pages it touches.
pub fn add_data_segments(wasm: &[u8], segments: &[(u64, &[u8])]) -> Result<Vec<u8>> {
    const DATA: u8 = 11;
    const DATA_COUNT: u8 = 12;
    if wasm.get(..8) != Some(b"\0asm\x01\0\0\0".as_slice()) {
        bail!("not a wasm module");
    }
    let mut out = wasm[..8].to_vec();
    let mut pos = 8;
    let mut added = false;
    while pos < wasm.len() {
        let id = wasm[pos];
        let (size, n) = read_uleb(&wasm[pos + 1..])?;
        let body = &wasm[pos + 1 + n..pos + 1 + n + size as usize];
        pos += 1 + n + size as usize;
        let new_body = match id {
            DATA => {
                let (count, n) = read_uleb(body)?;
                let mut b = Vec::new();
                write_uleb(&mut b, count + segments.len() as u64);
                b.extend_from_slice(&body[n..]);
                for (addr, bytes) in segments {
                    b.push(0); // active, memory 0
                    b.push(0x42); // i64.const
                    write_sleb(&mut b, *addr as i64);
                    b.push(0x0b); // end
                    write_uleb(&mut b, bytes.len() as u64);
                    b.extend_from_slice(bytes);
                }
                added = true;
                b
            }
            DATA_COUNT => {
                let (count, _) = read_uleb(body)?;
                let mut b = Vec::new();
                write_uleb(&mut b, count + segments.len() as u64);
                b
            }
            _ => body.to_vec(),
        };
        out.push(id);
        write_uleb(&mut out, new_body.len() as u64);
        out.extend_from_slice(&new_body);
    }
    if !added {
        bail!("the module has no data section");
    }
    Ok(out)
}

fn read_uleb(b: &[u8]) -> Result<(u64, usize)> {
    let mut v = 0u64;
    for (i, &byte) in b.iter().enumerate().take(10) {
        v |= ((byte & 0x7f) as u64) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    bail!("bad LEB128")
}

fn write_uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn write_sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        let done = (v == 0 && byte & 0x40 == 0) || (v == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::{Config, Engine, Instance, Linker, Memory, Module, Store, Val};

    /// Declarations every lifted module has (see lift/mod.rs).
    const PRELUDE: &str = r#"
target triple = "wasm64-unknown-unknown"
@R = global [32 x i64] zeroinitializer, align 8
@F = global [4 x i8] zeroinitializer
@V = global [32 x i128] zeroinitializer, align 16
@TP = global i64 0
@FPSR = global i32 0
%seg = type { i64, i64, i64, ptr }
declare void @__wasm_env_svc()
declare void @__wasm_env_fatal(ptr, i64)
declare void @__wasm_env_bad_target(i64)
declare void @dispatch(i64)
"#;

    /// A one-segment guest image at 0x400000: 16 bytes of data, 64 KB total.
    const IMAGE: &str = r#"
@seg0 = private constant [17 x i8] c"hello from svc!\0A\00"
@guest_segments = constant [1 x %seg] [%seg { i64 4194304, i64 16, i64 65536, ptr @seg0 }]
@guest_segment_count = constant i64 1
@guest_entry = constant i64 4194304
@guest_phdr = constant i64 4194560
@guest_phnum = constant i64 7
@guest_app_entry = constant i64 4194304
@guest_interp_base = constant i64 0
%lib = type { ptr, i64, i64 }
@guest_libs = constant [0 x %lib] zeroinitializer
@guest_lib_count = constant i64 0
@guest_objects = constant [0 x %lib] zeroinitializer
@guest_object_count = constant i64 0
"#;

    const IMAGE_END: u64 = 0x410000;

    /// Sets guest register `n` to the constant `v`.
    fn set(n: u32, v: i64) -> String {
        format!("  %p{n}_{v} = getelementptr [32 x i64], ptr @R, i64 0, i64 {n}\n  store i64 {v}, ptr %p{n}_{v}\n")
    }

    /// The dispatch table for a program whose only block is at guest_entry
    /// (0x400000). `dispatch` (in the support code) routes to it.
    const TABLE: &str = r#"
@dispatch_addrs = constant [1 x i64] [i64 4194304]
@dispatch_fns = constant [1 x ptr] [ptr @B_400000]
@dispatch_count = constant i64 1
"#;

    /// A program whose single block at the entry runs `body`, then exits
    /// with status 0 through svc.
    fn program(body: &str) -> String {
        format!(
            "{PRELUDE}{IMAGE}{TABLE}
define void @B_400000(i64 %_) {{
{body}{}  call void @__wasm_env_svc()
  unreachable
}}
",
            set(8, 94) + &set(0, 0)
        )
    }

    #[derive(Default)]
    struct Host {
        /// Every syscall: (number, first three arguments).
        calls: Vec<(i64, [i64; 3])>,
        /// Bytes written to fd 2.
        stderr: Vec<u8>,
        memory: Option<Memory>,
    }

    struct Run {
        store: Store<Host>,
        instance: Instance,
        memory: Memory,
    }

    impl Run {
        fn global(&mut self, name: &str) -> u64 {
            match self.instance.get_global(&mut self.store, name).unwrap().get(&mut self.store) {
                Val::I64(v) => v as u64,
                other => panic!("{name}: {other:?}"),
            }
        }
        fn u64_at(&self, addr: u64) -> u64 {
            let d = self.memory.data(&self.store);
            u64::from_le_bytes(d[addr as usize..addr as usize + 8].try_into().unwrap())
        }
        fn reg(&mut self, n: u64) -> u64 {
            let r = self.global("R");
            self.u64_at(r + n * 8)
        }
    }

    /// Builds `ir`, writes a Linux-style initial stack (argc, argv, envp,
    /// one aux entry) like the runtime does, and runs `_start` until the
    /// guest exits or traps. None if the toolchain isn't available.
    fn run(ir: &str, setup: impl FnOnce(&mut [u8])) -> Option<Run> {
        let Some(tc) = Toolchain::from_env() else {
            eprintln!("skipping: $WASM_CC not set (run inside `nix develop`)");
            return None;
        };
        static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let work = std::env::temp_dir().join(format!("wasm-env-link-test-{}-{n}", std::process::id()));
        let wasm = link(&tc, ir, IMAGE_END, &work).unwrap();
        let _ = std::fs::remove_dir_all(&work);

        let mut config = Config::new();
        config.wasm_memory64(true).wasm_tail_call(true).wasm_exceptions(true);
        let engine = Engine::new(&config).unwrap();
        let module = Module::new(&engine, &wasm).unwrap();
        let mut linker = Linker::new(&engine);
        linker
            .func_wrap(
                "linux",
                "syscall",
                |mut caller: wasmtime::Caller<'_, Host>, n: i64, a: i64, b: i64, c: i64, _d: i64, _e: i64, _f: i64| {
                    caller.data_mut().calls.push((n, [a, b, c]));
                    match n {
                        64 => {
                            let mem = caller.data().memory.unwrap();
                            let bytes = mem.data(&caller)[b as usize..(b + c) as usize].to_vec();
                            if a == 2 {
                                caller.data_mut().stderr.extend(bytes);
                            }
                            Ok(42i64)
                        }
                        94 => Err(wasmtime::Error::msg("exit")),
                        _ => Ok(-38i64),
                    }
                },
            )
            .unwrap();
        let mut store = Store::new(&engine, Host::default());
        let instance = linker.instantiate(&mut store, &module).unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        store.data_mut().memory = Some(memory);

        // argc=1, argv={"prog"}, envp={}, auxv={AT_PAGESZ 4096}, below the
        // module's stack pointer, as the runtime lays it out.
        let sp = match instance.get_global(&mut store, "__stack_pointer").unwrap().get(&mut store) {
            Val::I64(v) => v as u64,
            _ => unreachable!(),
        };
        let block = sp - 256;
        {
            let d = memory.data_mut(&mut store);
            d[block as usize + 128..block as usize + 133].copy_from_slice(b"prog\0");
            for (i, w) in [1u64, block + 128, 0, 0, 6, 4096, 0, 0].iter().enumerate() {
                let at = block as usize + i * 8;
                d[at..at + 8].copy_from_slice(&w.to_le_bytes());
            }
            setup(d);
        }
        let start = instance.get_typed_func::<i64, ()>(&mut store, "_start").unwrap();
        let _ = start.call(&mut store, block as i64);
        Some(Run { store, instance, memory })
    }

    #[test]
    fn content_keys_distinguish_contents_and_boundaries() {
        assert_eq!(content_key(&[b"ab", b"c"]), content_key(&[b"ab", b"c"]));
        assert_ne!(content_key(&[b"ab", b"c"]), content_key(&[b"a", b"bc"]), "boundaries count");
        assert_ne!(content_key(&[b"abc"]), content_key(&[b"abd"]));
        assert_eq!(content_key(&[b""]).len(), 32);
    }

    #[test]
    fn added_data_segments_are_placed_at_their_addresses() {
        // A memory64 module exporting its memory, with one data segment
        // ("ab" at 16) and a data count section.
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        wasm.extend([5, 3, 1, 0x04, 2]); // memory: 1, memory64, min 2 pages
        wasm.extend([7, 10, 1, 6]);
        wasm.extend(b"memory");
        wasm.extend([2, 0]); // export "memory"
        wasm.extend([12, 1, 1]); // data count 1
        wasm.extend([11, 8, 1, 0, 0x42, 16, 0x0b, 2, b'a', b'b']);
        let far = vec![7u8; 3];
        let out = add_data_segments(&wasm, &[(0x1_0000 + 5, &far), (300, b"xyz")]).unwrap();

        let mut config = Config::new();
        config.wasm_memory64(true);
        let engine = Engine::new(&config).unwrap();
        let module = Module::new(&engine, &out).unwrap();
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let d = memory.data(&store);
        assert_eq!(&d[16..18], b"ab", "existing segments are kept");
        assert_eq!(&d[300..303], b"xyz");
        assert_eq!(&d[0x1_0005..0x1_0008], &[7, 7, 7], "addresses above 2^16 (multi-byte LEB)");
        assert_eq!(d[303], 0);
    }

    #[test]
    fn slow_commands_are_stopped_at_the_limit() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        assert!(!run_limited(Command::new("sleep").arg("10"), Duration::from_millis(300)).unwrap(), "timed out");
        assert!(start.elapsed() < Duration::from_secs(5), "and killed, not waited for");
        assert!(run_limited(&mut Command::new("true"), Duration::from_secs(10)).unwrap());
        let err = run_limited(Command::new("sh").args(["-c", "echo broken >&2; exit 1"]), Duration::from_secs(10));
        assert!(err.unwrap_err().to_string().contains("broken"), "failures report stderr");
    }

    #[test]
    fn global_base_is_above_the_image() {
        assert_eq!(global_base(0x4b1234), 0x4c0000);
        assert_eq!(global_base(0x410000), 0x410000);
        assert_eq!(global_base(0), 0x10000);
    }

    #[test]
    fn maps_the_guest_image_and_zeroes_bss() {
        let Some(r) = run(&program(""), |d| d[0x400000 + 100] = 0xaa) else { return };
        let d = r.memory.data(&r.store);
        assert_eq!(&d[0x400000..0x400010], b"hello from svc!\n");
        assert_eq!(d[0x400000 + 100], 0, ".bss is zeroed even if memory wasn't");
    }

    #[test]
    fn builds_the_guest_initial_stack() {
        let Some(mut r) = run(&program(""), |_| {}) else { return };
        let sp = r.reg(31);
        assert_eq!(sp % 16, 0);
        let stack_pointer = r.global("__stack_pointer");
        assert!(sp < stack_pointer, "a separate region, not the runtime's block");
        assert_eq!(r.u64_at(sp), 1, "argc");
        let argv0 = r.u64_at(sp + 8);
        assert_eq!(&r.memory.data(&r.store)[argv0 as usize..argv0 as usize + 4], b"prog", "strings are shared");
        assert_eq!(r.u64_at(sp + 16), 0, "argv NULL");
        assert_eq!(r.u64_at(sp + 24), 0, "envp NULL");
        let mut aux = Vec::new();
        let mut p = sp + 32;
        while r.u64_at(p) != 0 {
            aux.push((r.u64_at(p), r.u64_at(p + 8)));
            p += 16;
        }
        assert_eq!(aux, [(6, 4096), (3, 0x400100), (4, 56), (5, 7), (9, 0x400000)], "runtime's entries, then ELF ones");
    }

    #[test]
    fn svc_passes_arguments_and_returns_x0() {
        let body = set(8, 64) + &set(0, 1) + &set(1, 0x400000) + &set(2, 16)
            + "  call void @__wasm_env_svc()\n"
            + "  %x0 = load i64, ptr @R\n"
            + "  %p20 = getelementptr [32 x i64], ptr @R, i64 0, i64 20\n"
            + "  store i64 %x0, ptr %p20\n";
        let Some(mut r) = run(&program(&body), |_| {}) else { return };
        assert_eq!(r.store.data().calls[0], (64, [1, 0x400000, 16]));
        assert_eq!(r.reg(20), 42, "the host's result lands in x0");
        assert_eq!(r.store.data().calls.last().unwrap().0, 94, "then exit_group");
    }

    #[test]
    fn clone_is_refused_without_reaching_the_host() {
        let body = set(8, 220)
            + "  call void @__wasm_env_svc()\n"
            + "  %x0 = load i64, ptr @R\n"
            + "  %p20 = getelementptr [32 x i64], ptr @R, i64 0, i64 20\n"
            + "  store i64 %x0, ptr %p20\n";
        let Some(mut r) = run(&program(&body), |_| {}) else { return };
        assert_eq!(r.reg(20) as i64, -38, "ENOSYS");
        assert!(r.store.data().calls.iter().all(|c| c.0 != 220));
    }

    #[test]
    fn fatal_errors_name_the_guest_address() {
        let ir = format!(
            "{PRELUDE}{IMAGE}{TABLE}
@msg = private constant [22 x i8] c\"unsupported `foo x0`\\00\\00\"
define void @B_400000(i64 %_) {{
  call void @__wasm_env_fatal(ptr @msg, i64 4194308)
  unreachable
}}
"
        );
        let Some(r) = run(&ir, |_| {}) else { return };
        let msg = String::from_utf8_lossy(&r.store.data().stderr).into_owned();
        assert!(msg.contains("unsupported `foo x0`"), "{msg}");
        assert!(msg.contains("0x0000000000400004"), "{msg}");
    }

    #[test]
    fn jumps_to_untranslated_addresses_are_reported() {
        let ir = format!(
            "{PRELUDE}{IMAGE}{TABLE}
define void @B_400000(i64 %_) {{
  musttail call void @dispatch(i64 12345)
  ret void
}}
"
        );
        let Some(r) = run(&ir, |_| {}) else { return };
        let msg = String::from_utf8_lossy(&r.store.data().stderr).into_owned();
        assert!(msg.contains("wasn't translated at 0x0000000000003039"), "{msg}");
    }

    #[test]
    fn the_module_has_the_wasm_env_abi_shape() {
        let Some(mut r) = run(&program(""), |_| {}) else { return };
        let heap = r.global("__heap_base");
        assert!(heap >= IMAGE_END, "the mmap region starts above the image");
    }
}
