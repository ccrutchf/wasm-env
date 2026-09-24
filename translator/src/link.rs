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
    &["--target=wasm64-unknown-unknown", "-mtail-call", "-mbulk-memory", "-mexception-handling"];

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

/// Compiles and links `ir` with the support code into a WebAssembly
/// module, using `work` for intermediate files.
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
%seg = type { i64, i64, i64, ptr }
declare void @__wasm_env_svc()
declare void @__wasm_env_fatal(ptr, i64)
declare void @__wasm_env_bad_target(i64)
"#;

    /// A one-segment guest image at 0x400000: 16 bytes of data, 64 KB total.
    const IMAGE: &str = r#"
@seg0 = private constant [17 x i8] c"hello from svc!\0A\00"
@guest_segments = constant [1 x %seg] [%seg { i64 4194304, i64 16, i64 65536, ptr @seg0 }]
@guest_segment_count = constant i64 1
@guest_entry = constant i64 4194304
@guest_phdr = constant i64 4194560
@guest_phnum = constant i64 7
"#;

    const IMAGE_END: u64 = 0x410000;

    /// Sets guest register `n` to the constant `v`.
    fn set(n: u32, v: i64) -> String {
        format!("  %p{n}_{v} = getelementptr [32 x i64], ptr @R, i64 0, i64 {n}\n  store i64 {v}, ptr %p{n}_{v}\n")
    }

    /// A program whose single block at the entry runs `body`, then exits
    /// with status 0 through svc.
    fn program(body: &str) -> String {
        format!(
            "{PRELUDE}{IMAGE}
define void @dispatch(i64 %pc) {{
  switch i64 %pc, label %bad [ i64 4194304, label %entry ]
entry:
{body}{}  call void @__wasm_env_svc()
  unreachable
bad:
  call void @__wasm_env_bad_target(i64 %pc)
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
            "{PRELUDE}{IMAGE}
@msg = private constant [22 x i8] c\"unsupported `foo x0`\\00\\00\"
define void @dispatch(i64 %pc) {{
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
            "{PRELUDE}{IMAGE}
define void @dispatch(i64 %pc) {{
  switch i64 %pc, label %bad [ i64 4194304, label %entry ]
entry:
  musttail call void @dispatch(i64 12345)
  ret void
bad:
  call void @__wasm_env_bad_target(i64 %pc)
  unreachable
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
