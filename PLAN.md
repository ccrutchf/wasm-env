# wasm-env

A Linux environment whose native architecture is WebAssembly. Software is compiled to it directly; existing x86-64 and arm64 binaries and OCI images are translated to it ahead of time. It runs on any machine, whatever its OS or CPU architecture.

## Problem

In a BYOD environment people use different operating systems (macOS, Windows, Linux) on different architectures (Apple Silicon arm64, x86_64).

Docker solves the OS half of this but not the architecture half. A container image is still native machine code: every binary must be built once per architecture, and multi-arch images just bundle those builds together. A team with Apple Silicon Macs and x86 Windows machines has to build, test and publish everything twice. Emulating the other architecture is slow and fragile.

## Idea

**WASM is our native format.** We define a Linux ABI for WASM: a module that makes Linux syscalls through an import. Software built for wasm-env is compiled to that ABI from source. A native runtime on each host implements the syscalls, hiding the host OS and CPU.

Existing software reaches the same ABI through ahead-of-time translation. A converter pulls an OCI image, uses recompiled packages where they exist, translates the remaining x86-64 or arm64 binaries to WASM, and produces a `wasi/wasm` image. This is the Rosetta 2 model: translate once, cache, run everywhere.

Both paths produce the same kind of module and run on the same runtime, so there is one system.

## Goals

- **Primary workload: Ubuntu images running existing Python wheels.** Unmodified manylinux wheels from PyPI, including C extensions like numpy and pandas, and eventually PyTorch (CPU). Ubuntu 24.04 LTS, arm64 images first.
- Existing OCI images convert to a single `wasi/wasm` image that runs identically on macOS arm64, Windows x86_64 and Linux x86_64.
- Linux-compatible environment: shell, processes, pipes, filesystem, env vars, exit codes.
- Runs on a native runtime on each host (not in a browser).
- Research environment first; grows workload by workload.
- Learning project first, then possibly a product.

## Non-goals (for now)

- GUI.
- GPU. Research for much later: running CUDA kernels through wgpu (see "GPU research" below).
- JIT-based runtimes (Node/V8, JVM). They generate machine code at runtime, which ahead-of-time translation can't see; revisit later. Libraries with an internal JIT (oneDNN in PyTorch) must run with it disabled.
- Multithreading, until the PyTorch milestone (see Milestones). numpy and pandas run single-threaded (`OPENBLAS_NUM_THREADS=1`).

## Approach: hybrid

For each binary, use the first path that works:

1. **Recompile to WASM (ideal — our native format).** Build from source against the wasm-env Linux ABI. Most images are Alpine or Debian underneath, so the converter swaps in recompiled packages (for example `bash`, `python3`) where they exist. Fastest and most faithful result.
2. **Ahead-of-time translation from another ISA (backup).** Translate the ELF binary to WASM at conversion time and cache it. A runtime fallback handles only code that can't be found ahead of time. Two source ISAs, each with a different strength:
   - **x86-64 — best compatibility.** Nearly every image and binary exists for x86-64, and many exist only for it.
   - **arm64 — easiest to translate.** Fixed-width instructions make finding code far easier, and its weaker memory ordering maps onto WASM without extra fences.

   Start with arm64 to prove the translator, then add x86-64 for coverage. Both frontends lift to the same LLVM IR, so everything after lifting is shared.
3. **CPU emulation (last resort).** Emulate the CPU inside WASM (as container2wasm does). Slow (5–50×), but it keeps things working. Avoid where possible.

### One world per process

Code inside a process shares a libc, `malloc`, data structures and calling conventions, so **each process is either entirely native (recompiled, musl) or entirely translated (from the image, usually glibc).** Processes only talk through syscalls, so mixing across process boundaries is fine: a native shell can start a translated `python3`.

This is why we don't port glibc. A Python that loads manylinux wheels must be a translated process: Ubuntu's own `python3`, its glibc and `libstdc++`, and the wheels' `.so` files, all translated from arm64. glibc arrives with it as ordinary translated code. musl remains the libc for recompiled software.

Recompiled wheels (built for our platform, like Pyodide does for its WebAssembly Python) are a later speed-up for the hottest packages. They would need a native Python and a wheel build farm.

## Architecture

```
 OCI image (arm64)                                   wasi/wasm OCI image
 ┌──────────────┐    ┌────────────────────────┐     ┌──────────────┐
 │ layers       │───▶│ converter              │────▶│ layers       │
 │  ELF, .so    │    │  - unpack, classify    │     │  .wasm       │
 │  scripts     │    │  - package substitution│     │  scripts     │
 │  data        │    │  - translator          │     │  data        │
 └──────────────┘    │  - translation cache   │     └──────┬───────┘
                     └────────────────────────┘            │
                                                           ▼
                                  ┌────────────────────────────────────┐
                                  │ runtime (wasmtime embedding)       │
                                  │  - Linux syscall layer             │
                                  │  - process model: fork/exec/wait   │
                                  │  - virtual filesystem + host mounts│
                                  └────────────────────────────────────┘
                                      macOS arm64 · Windows x86_64 · Linux
```

### Converter

1. **Pull** the image. Prefer the arm64 variant when one exists (easier to translate); use x86-64 when it's the only one.
2. **Unpack** the layers and classify each file: ELF executables and shared libraries go to translation; scripts, configs and data are copied unchanged.
3. **Substitute** recompiled WASM packages where available, using the image's package database (apk, dpkg).
4. **Translate** the remaining ELF files.
5. **Output** an OCI image with platform `wasi/wasm`, preserving the layer structure.

### Native ABI and toolchain

The full specification is [docs/abi.md](docs/abi.md) (version 0). Summary:

- A memory64 WASM module that exports `_start` and imports a single Linux syscall function, roughly `linux.syscall(nr, a0..a5) -> i64`, using Linux syscall numbers and the 64-bit (LP64) struct layouts. One ABI for recompiled and translated software; no 4 GB per-process limit.
- A musl libc port whose syscall wrapper calls that import, plus a clang/LLVM toolchain targeting it (`wasm64-linux-musl`). Porting software means cross-compiling it with this toolchain.
- The translator emits modules with the same imports, so recompiled and translated binaries are indistinguishable to the runtime.

### Translator

- Lift machine code to LLVM IR (remill/McSema or rev.ng style), then compile to WASM with LLVM's WASM backend.
- Find code by recursive disassembly from entry points and symbols. Indirect jumps go through a dispatch table of known targets. Unknown targets trap to a runtime translation fallback.
- Use memory64 (standardized in Wasm 3.0) so translated 64-bit pointers work unchanged.
- **Dynamic linking, not just link-time merging.** Python loads wheels with `dlopen`, and `pip install` adds new `.so` files after the image was converted. So each shared object translates to its own WASM module, and the runtime loads and links them at run time: all modules share one memory and one function table, and the runtime's loader resolves symbols (including glibc's symbol versions). This needs ABI version 1 (docs/abi.md is static-only today).
- **Translate at first load, cache by file hash** (the Rosetta 2 model). Image conversion pre-translates what's in the image; anything added later translates the first time it's loaded. Layers are shared across images, so a given glibc or `libpython` is translated once.
- **SIMD and CPU features.** arm64's NEON is 128-bit, the same width as WASM SIMD, so numpy's and OpenBLAS's NEON kernels translate closely. Libraries choose kernels by reading `AT_HWCAP` (arm64) or `cpuid` (x86-64); the runtime advertises exactly what translates well (NEON yes, SVE no; on x86-64, SSE up to 4.2, not AVX).
- **C++ exceptions.** Translated C++ unwinds with DWARF `.eh_frame` data through `libgcc_s`. Throwing through translated frames needs either a translated unwinder that understands translated frames, or a mapping onto WASM exceptions. PyTorch needs this; numpy and pandas mostly don't.

### Runtime

- Translated binaries carry their own libc, which makes raw Linux syscalls. The runtime therefore implements the **Linux syscall interface**, like WSL1 and gVisor, not a POSIX library layer. Recompiled packages target the same interface, so there is one system underneath both.
- Built as a **wasmtime** embedding, with the syscall layer as host functions. We're writing the syscall layer ourselves, so we don't need WASIX's POSIX layer. wasmtime is standards-track and has memory64 support. We'll still study WASIX for how it implements fork and signals.
- Emulates fork, execve, pipes, signals and `mmap` on top of WASM.
- Mounts the image filesystem, with explicit host directory mounts.
- Runs each environment in its own OS process, and compiles 64-bit memories with masked addressing instead of bounds checks (see M-1).

## Implementation

- **Rust** for all components: converter, translator driver and runtime. wasmtime is a Rust library, so the runtime embeds it directly.
- Toolchain pieces that already live in LLVM (the WASM backend, the musl port's build) stay C/C++ and are driven from Rust.
- Useful crates: `wasmtime` (runtime), `oci-client` (registry pull/push), `object` or `goblin` (ELF parsing), `inkwell` (LLVM bindings, if the lifter is ours).

## Distribution

**The runtime** is the only per-architecture artifact in the whole system: one Rust binary (`wasm-env`) with wasmtime embedded. It ships as a prebuilt, self-contained binary per host (macOS arm64/x86-64, Windows x86-64/arm64, Linux x86-64/arm64). Channels: GitHub releases with an install script, Homebrew, winget/Scoop, and a Nix package. Nix can't be a requirement: BYOD users on macOS and Windows won't have it.

**The Nix flake** is our development environment and a reproducible build of the release binaries. It is not how end users install wasm-env.

**Environments** (converted or natively built images) are OCI images with a single `wasi/wasm` platform. We reuse existing registries, tags and signing, with no Docker required at runtime. One image serves every host.

## Performance targets (draft)

- Translated binaries within ~2–3× of native; recompiled packages within ~1.5×.
- Environment startup < 100ms, with cached compiled modules.
- Converting an image is a one-time cost; minutes is acceptable at first.

## Milestones

- **M-1 — wasm32 vs memory64 benchmarks (done on Linux x86-64).** Decided the native ABI's memory model: memory64 with masked addressing, ~1.2× native (~1.12× excluding interpreters), on wasmtime. See [Benchmarks](#m-1-benchmarks) below.
- **M0 — Native ABI (done on Linux x86-64).** Specified in [docs/abi.md](docs/abi.md). musl 1.2.6 ported to `wasm64` (`toolchain/musl-overlay`), compiler-rt builtins, a `wasm64-linux-musl-cc` wrapper, and the `wasm-env` runtime (`runtime/`, about 50 syscalls). A C test program and BusyBox 1.37 (44 applets, 294 KB) pass `tests/m0.sh` (20 checks). Still to do: run it on the Mac and on Windows. See [M0 notes](#m0-notes).
- **M1 — Processes and a shell (done on Linux x86-64).** `vfork` (built on setjmp and WebAssembly exceptions) + `execve`, `posix_spawn`, `system`/`popen`, `wait4`, pipes, `kill` with default signal actions, `#!` scripts and virtual `/dev` devices. BusyBox in its no-MMU mode with the `hush` shell. `tests/m1.sh` passes 27 checks. See [M1 notes](#m1-notes).
- **M2 — arm64 translation, static (done on Linux x86-64).** A Rust translator (`translator/`): ELF loading, block discovery (branch/pointer targets plus GCC/LLVM jump-table recognition), an arm64→LLVM-IR lifter (`lift/`, integer + scalar FP + a NEON subset, blocks chained by `musttail`, indirect branches through a dispatch switch), and a link step producing a wasm-env program. Six C fixtures (from a libc-free program up to printf/malloc/setjmp and SIMD string code) run **byte-identical to qemu-aarch64** via `tests/m2.sh`. See [M2 notes](#m2-notes).
- **M3 — Translated dynamic linking (in progress).** Slice 1 (done): position-independent executables (ET_DYN). The loader assigns a fixed base, applies relative and defined-symbol relocations at translation time (prelinking), so the 1:1 address model holds. static-PIE fixtures pass differentially vs qemu (`tests/m2.sh`). Slice 2 (done): multi-object prelinking — an executable and its shared libraries are laid out at non-overlapping bases, cross-object symbols and relocations resolved at translation time, and merged into one module. A two-object dynamic program matches qemu (`tests/m3.sh`). Slice 3 (in progress): a real dynamically-linked musl program (main + full `libc.so`). The link step works — symbols resolve, weak-undefined resolve to 0, relocations apply, valid IR is produced — and **chunked compilation now makes it build**: the lifted functions are split across many small modules compiled in parallel, and all control flow routes through a binary-search `dispatch` (in the support code) over a block table, so there is no giant switch. The full 1 MB `libc.so` compiles and links (a 5.6 MB module) with bounded memory. It runs, too: with a thread pointer provided, the translated program reaches `main` and prints its output (relocations and cross-object calls all correct). Full correctness still needs the two things ld.so does that prelinking doesn't: (1) multi-module static-TLS setup — combine each object's PT_TLS into one block, set the thread pointer, resolve TPREL relocations — and (2) running `init_array` constructors. Those are the next slice. Then `dlopen` at runtime (ABI version 1: modules sharing one memory and function table). Goal: Ubuntu 24.04 arm64's own `python3 -c 'print(1)'`, with its glibc, running translated.
- **M4 — Python wheels.** `pip install` into the translated Python and import binary wheels: first a small C extension (`markupsafe`), then **numpy** (with bundled OpenBLAS, single-threaded), then **pandas**. Measure against native arm64.
- **M5 — Ubuntu images.** `wasm-env convert ubuntu:24.04` (arm64): pre-translate the image, keep dpkg metadata, and `wasm-env run` gives a shell in it. Networking (sockets, DNS) so `pip` can reach PyPI.
- **M6 — PyTorch (CPU).** Threads (ABI version 2: shared memory, atomics, `clone`), C++ exceptions through translated code, and oneDNN's JIT disabled. Goal: `import torch` and a small model's forward pass.
- **Later.** Native recompiled userland and recompiled wheels for hot packages (nixpkgs cross-compilation; Pyodide recipes); x86-64 translation for x86-only images; runtime translation fallback; CPU emulation fallback; GPU (below).

## Testing

Test-first from M2 on. `nix develop -c tests/all.sh` runs everything:

| Suite | What it covers |
|---|---|
| `cargo test -p wasm-env` (35 tests) | Runtime units: process table (wait, kill, orphans, groups), exec resolution and `#!` scripts, the initial stack layout, signal dispositions, descriptor tables, path resolution, stat layouts, memory helpers, pipes |
| `cargo test -p wasm-env-translate` (20 tests) | ELF loading, block discovery (branch targets, return addresses, pointer-only functions, data in the code segment), and the support code and link step, run in wasmtime with a fake syscall import |
| `tests/m0.sh`, `tests/m1.sh` | End to end: the toolchain, BusyBox, the shell, processes and pipes |
| `tests/m2.sh` | **Differential testing of the translator**: C fixtures in `tests/arm64/` built for arm64, run under qemu-aarch64 as the reference and translated under wasm-env; output and exit status must match exactly |

The arm64 fixtures are plain C, not assembly: gcc at `-O2` produces the instruction patterns real binaries contain, and each fixture prints its results. `tests/m2.sh` fails until `translator/src/lift/insn.rs` is complete.

## M-1 benchmarks

**Question:** how much slower is memory64 than wasm32, and is it worth having one ABI for recompiled and translated code?

**Why it might be slower.** For wasm32, runtimes reserve 4 GB of virtual memory plus guard pages, so out-of-bounds accesses fault in hardware and most bounds checks disappear. memory64 can't use that trick, so every load and store gets an explicit bounds check. 64-bit pointers also use more cache.

**Setup**

- The same C sources compiled three ways: native (x86-64 / arm64), `wasm32` and `wasm64`, all `-O2`.
- Freestanding (no libc) with a tiny bump allocator, so libc support for wasm64 isn't a variable.
- A Rust harness embeds wasmtime, instantiates each module, and times only the exported `run()` call. It reports the median of N runs, compiled code size and compile time.

**Workloads**

| Benchmark | What it stresses |
|---|---|
| Linked-list / binary-tree walk | Pointer size, cache footprint |
| Array sum / memcpy | Streaming loads and stores (bounds checks) |
| Sort (quicksort) | Mixed compute and memory |
| Hash table insert/lookup | Random access |
| Bytecode interpreter loop | Dispatch-heavy code, like bash and Python |
| SHA-256 | Pure compute (control; should be equal) |

**Configurations:** native; wasm32 with guard pages (default); wasm32 with explicit bounds checks (isolates the bounds-check cost); memory64.

**Hosts:** Linux x86-64, macOS arm64, Windows x86-64.

**Decision rule:** the original draft compared memory64 against wasm32 (≤ 15% overhead). That was the wrong baseline: what users feel is the cost versus native. The rule is now that recompiled software stays within the 1.5× of native performance target.

### Results: Linux x86-64 (2026-09-23)

Intel i7-11800H, wasmtime 49 (with our patch for `wasm64-masked`), clang 21, wabt 1.0.41, `-O2`. The harness pins itself to one core and interleaves configurations; 9 runs each. Load average was ~2 during the run. Spread (interquartile range) was under 5% for most results, but 8–20% for `list` and `tree`, which are dominated by memory latency. Raw data: `bench/results/linux-x86_64.json`.

Times relative to native (lower is faster):

| Benchmark | wasm32 | wasm64 | wasm64-nospectre | **wasm64-masked** | w2c-wasm32 | w2c-wasm64 | w2c-wasm64-masked |
|---|---|---|---|---|---|---|---|
| list | 0.47× | 1.03× | 1.04× | **1.10×** | 0.46× | 0.98× | 1.10× |
| tree | 0.66× | 1.19× | 1.02× | **1.06×** | 0.67× | 1.10× | 1.12× |
| stream | 0.99× | 1.08× | 1.04× | **1.02×** | 1.04× | 1.00× | 1.05× |
| sort | 1.06× | 1.48× | 1.33× | **1.27×** | 1.15× | 1.41× | 1.16× |
| hash | 1.00× | 1.16× | 1.03× | **1.08×** | 1.05× | 1.02× | 1.07× |
| interp | 2.29× | 2.84× | 2.19× | **1.96×** | 2.25× | 2.93× | 2.10× |
| sha256 | 1.13× | 1.44× | 1.24× | **1.21×** | 1.23× | 1.45× | 1.22× |
| **geomean** | **0.98×** | **1.38×** | **1.22×** | **1.21×** | **1.01×** | **1.31×** | **1.22×** |

`wasm*` columns are wasmtime (Cranelift). `w2c-*` columns are the same `.wasm` files translated to C by wasm2c and compiled by clang (LLVM), with the same memory strategies.

**Findings**

1. **Masked memory64 is ~1.21× native, and ~1.12× excluding the interpreter.** Bounds checks are no longer a meaningful cost.
2. **The compiler backend doesn't matter.** LLVM (via wasm2c) is no faster than Cranelift in any memory mode (masked: 1.22× vs 1.21×). We stay on wasmtime.
3. **What's left is WASM's structure, not memory64:**
   - **Interpreter dispatch (~2×).** Native clang builds "threaded" dispatch: the indirect jump is copied into every opcode handler (10 dispatch points) with no range check, so the CPU predicts each handler's next opcode separately. In WASM, a C `switch` in a loop becomes one `br_table`. Structured control flow sends every handler back to that single dispatch point, and `br_table` always needs a default branch, so the range check stays. Neither Cranelift nor clang rebuilds the threaded form.
   - **Local arrays live in linear memory (`sha256`, `sort`, ~1.2×).** Locals that need an address, like `sha256`'s `w[64]`, go on a shadow stack inside WASM memory, so each access takes the memory path instead of a direct stack access.
   - wasm2c also reloads the memory base address before every access, because clang can't prove stores to WASM memory don't overwrite it. wasm2c's "Segue" mode (base kept in the `gs` register) fixes that, but since LLVM isn't helping, it isn't worth pursuing now.
4. **Pointer size (list, tree).** wasm32's 32-bit pointers make it 1.5–2× *faster* than native on pointer-heavy code (like Linux's x32 ABI). memory64 and native both pay for 64-bit pointers.

**Decision: memory64 everywhere, with masked addressing, on wasmtime.** Real workloads regularly need more than 4 GB in a single process, which rules out wasm32 as the default. With masking, memory64 is ~1.2× native, inside the 1.5× target, and one ABI keeps the syscall layer simple. wasm32 remains a possible opt-in "compact" build for small tools if a real workload justifies it. Still to do: confirm on macOS arm64 and Windows x86-64, and rerun on an idle machine.

**Closing the remaining gap (research, later):**
- **Interpreters:** teach Cranelift to duplicate `br_table` dispatch into loop predecessors, rebuilding threaded dispatch. Or build interpreters with WASM tail calls (`return_call_indirect`) so each handler jumps directly to the next, the way CPython 3.14's tail-calling interpreter works natively.
- **Shadow stack:** investigate keeping more locals out of linear memory, in both LLVM's WASM backend and Cranelift.

### Masked addressing

Our runtime patches wasmtime's compiler (`vendor/wasmtime-internal-cranelift`, search for "wasm-env") so that 64-bit memories are masked instead of bounds-checked:

```
addr = base + (index & (2^38 - 1)) + offset
```

- Each memory gets a fixed 2^38-byte (256 GB) virtual reservation that never moves, plus a guard region covering the largest static offset. That limits each process to 256 GB of memory.
- Pages beyond the memory's current size are inaccessible, so **out-of-bounds accesses below 256 GB still trap**. Only indexes at or above 2^38 wrap around. This deviates from the WebAssembly spec, but not from safety: every access stays inside the process's own reservation, even speculatively, so no Spectre guard is needed.
- This is the same technique Google's Native Client used on x86-64, and it mirrors native amd64, which also has a hardware-capped address space (48 bits).
- A test in `bench/harness` checks all three behaviors: in-bounds loads, trapping below 2^38, and wrapping above.
- Before production: turn the environment-variable switch into a proper wasmtime configuration option, get the patch reviewed (the file warns it is safety-critical), and consider proposing it upstream.

### Security model: one OS process per environment

Native code gets memory isolation for free from the CPU's memory hardware, but only between processes. WASM enforces its sandbox in software because the guest shares a process with the runtime. We put the trust boundary where the hardware enforces it:

- **Each environment runs in its own OS process.** Another environment's data, and the host's, is never in the same address space.
- **Bounds enforcement stays** (masking plus guard pages) to protect the runtime's own state inside the process. The runtime holds the environment's host privileges (file handles, the ability to make real syscalls), so guest code must not be able to overwrite it.
- **Spectre guards aren't needed.** The process holds nothing the environment couldn't already reach, so speculative reads leak nothing new. Masked addresses can't leave the reservation even speculatively.
- Separate processes also give crash isolation, and they let us use OS resource limits for each environment.


## M0 notes

**Layout**

| Path | What |
|---|---|
| `docs/abi.md` | The ABI specification (version 0) |
| `toolchain/musl-overlay/` | Our musl port: `arch/wasm64` plus a few `src/*/wasm64` files. Upstream musl stays pristine; `toolchain/build.sh` copies the overlay onto it |
| `toolchain/build.sh` | Builds `build/sysroot`: musl and compiler-rt builtins |
| `toolchain/bin/wasm64-linux-musl-cc` | The cross compiler: clang plus our flags, sysroot and link line |
| `runtime/` | The `wasm-env` runtime: wasmtime with our masking patch, startup, the mmap allocator, filesystem and syscalls |
| `packages/busybox/` | BusyBox build script and applet list |
| `tests/m0.sh` | End-to-end acceptance test |

**Findings**

- **The musl port is small.** About 150 lines: syscall, thread-pointer, atomics and startup headers, plus setjmp/longjmp, `__set_thread_area` and `__unmapself`. Everything else builds from musl's generic C code.
- **setjmp/longjmp work through WebAssembly exception handling** (`-mllvm -wasm-enable-sjlj`, exnref form), with three small helper functions in libc. wasmtime 49 supports it.
- **clang renames `main` on WebAssembly** (`__main_argc_argv` or `__main_void`), and an indirect call with the wrong signature traps. `crt1` adapts, using strong references so `main` is found even inside a static library (BusyBox keeps it in `libbb/lib.a`).
- **compiler-rt bug:** its soft-float comparison functions return `long` on every 64-bit target except arm64, but LLVM's WebAssembly backend expects `int`. We patch it at build time; worth reporting upstream.
- **Build-system friction is small.** The wrapper passes partial links (`-r`) through and drops GNU ld flags that `wasm-ld` rejects (`--start-group`, `--warn-common`). BusyBox then built unmodified.
- **Compiled programs are cached.** Without a cache, Cranelift recompiled BusyBox on every run (0.8 s of CPU for `true`). With wasmtime's cache in our own directory, `true` takes 7 ms.
- **Paths are resolved by the runtime, not the host.** Symlinks, including absolute ones, resolve inside the environment's root, and `..` can't climb out.

**Known gaps (for M1 and later)**

- No `fork`, `execve`, pipes or signals, so no shell yet. This is M1.
- Terminal size is fixed at 80×24; there's no terminal mode control (`TCGETS`/`TCSETS`).
- Paths must be valid UTF-8; file-backed `mmap` is read-only.
- Not yet run on macOS or Windows. The Windows-specific code (symlinks, permissions, `stat`) has never been compiled.

## M1 notes

**How processes work**

- **Each guest process is a host thread** with its own wasmtime instance, all in the environment's one host process (the security model). Pipes, `wait4` and signals are in-runtime data structures, so they work the same on every host, and starting a process is cheap: the compiled program is shared, and instantiation takes microseconds.
- **vfork is built on setjmp.** The child runs in the parent's memory until it calls `execve` or `_exit`; the runtime then starts the new program (or records the exit) and returns the child's pid, and libc `longjmp`s back to the `vfork` call site. This is real vfork semantics, and it's cheap because it reuses the WebAssembly-exception setjmp from M0. Details in docs/abi.md, "Processes".
- **BusyBox runs in its no-MMU mode**, built for hardware without `fork`: it uses only `vfork`+`exec`, and re-executes itself where it would otherwise fork. Its `hush` shell supports that mode fully.

**Findings**

- **Deadlock in the first version of `wait4`**: it held the process-table lock while checking for signals, which takes the same lock. Rust's `Mutex` isn't re-entrant. Found by reading thread states from `/proc` (ptrace is blocked on this machine).
- **argv is bytes, not text.** BusyBox marks a re-executed process by setting the high bit of `argv[0][0]`, which isn't valid UTF-8. The runtime now carries argv and envp as raw bytes.
- **Variadic `int` arguments arrive with garbage in the high 32 bits** on wasm64 (musl reads `fcntl`'s argument as a `long`). The runtime truncates them to 32 bits, as the Linux kernel does.
- **BusyBox bug in no-MMU `timeout`**: it re-executes itself with `-p PID` after the duration, where getopt never sees it. We carry a small patch (`packages/busybox/patches/`); worth sending upstream.

**Known gaps**

- **No `fork`.** Programs that fork without exec fail with `ENOSYS`. The options are in docs/abi.md.
- **Signal handlers never run**: they're recorded, and a signal with a handler is ignored. Needs a way to interrupt a running instance and call into it (for example with wasmtime's epoch interruption).
- **A process in a CPU loop can't be killed** until its next syscall. The same interruption mechanism would fix it.
- **No terminal control** (`TCGETS`/`TCSETS`, process-group job control), so no interactive line editing yet.

## GPU research (much later)

The eventual wish: run CUDA kernels from existing PyTorch wheels on any host's GPU through wgpu (Vulkan, Metal, DirectX 12). Two possible shapes:

- **A CUDA driver inside the runtime.** Implement `libcuda`/`libcudart` as runtime services: memory management, streams and kernel launches. Translate each kernel's PTX to WGSL or SPIR-V and run it with wgpu. ZLUDA and chipStar show that translating PTX works. The hard part is cuBLAS and cuDNN: they are closed binaries, often without PTX, so their APIs would need our own implementations on top of wgpu. WGSL's limits (no 64-bit pointers, limited atomics, little fp64) are a further constraint.
- **A PyTorch device backend on wgpu** (PyTorch's `PrivateUse1` extension point), bypassing CUDA entirely. Less general, but more achievable.

Either way the runtime would expose the GPU to the process through host functions, the way FEX-Emu's "thunks" hand some library calls to native host implementations.

## M2 notes

**How translation works** (see also `docs/abi.md` and the module docs)

- **Discovery** (`translator/src/discover.rs`) decodes every 4-byte word in the executable sections, then finds basic blocks by following control flow from roots: the entry point, function symbols, and addresses formed as code pointers (ADR/ADRP+ADD, or stored in data). It recognizes the GCC/LLVM **jump-table** idiom (`adrp`+`add` table base, `ldr{b,h,sw}` index, `adr` label, `add …, ext #2`, `br`) and reads the offset table, so `switch` targets are found without executing anything.
- **Lifting** (`translator/src/lift/`) turns each block into an LLVM function. Guest registers live in globals (`@R`, `@F` flags, `@V`, `@TP`), cached as SSA values within a block and flushed at its end. Guest memory *is* the WebAssembly memory: guest address A is WebAssembly address A, so the masked-memory64 runtime bounds it for free. Blocks pass control by **guaranteed tail call** (`musttail` → `return_call`), so the host stack never grows however the guest branches, calls or returns; indirect branches (BR/BLR/RET) go through a `@dispatch` switch over every block start.
- **Linking** (`translator/src/link.rs`) compiles the IR and the support code (`translator/support/runtime.c`) for wasm64 with clang, and links them with wasm-ld plus the compiler-rt builtins (`__multi3` etc., which LLVM emits for 128-bit arithmetic and divide-by-constant). The support code maps the guest image at its original addresses, builds the Linux initial stack (adding `AT_PHDR`/`AT_ENTRY`), and forwards `svc` to the `linux.syscall` import — so translated and recompiled programs make identical syscalls.

**Coverage.** Integer arithmetic/logic/shifts/bitfields/conditionals, all load/store forms (including pairs, exclusives as single-threaded, and `ld1`/`st1`), scalar floating point (arithmetic, compare, convert, fma, select), and a NEON integer subset (element-wise arithmetic and compares, `dup`/`movi`, `cnt`, reductions, shifts, permutes, narrowing, element moves). Cache-maintenance ops are no-ops. Anything unimplemented lifts to a trap that names the instruction and address, so gaps surface at runtime rather than as wrong answers.

**Findings**

- **bad64 names x29/x30 `fp`/`lr`**; the register decoder handles those aliases.
- **Divide-by-constant introduces 128-bit multiply.** LLVM strength-reduces `x / 10` to a 64×64→128 multiply, so translated programs need compiler-rt's `__multi3`; the sysroot's builtins archive is linked in.
- **Vector results are always full i128.** `vbuild` zero-fills unused high lanes (the arm64 rule that writing a 64-bit vector result clears the upper half), so results are written back as 128 bits.
- **Jump tables reuse the base register.** `ldrh w1, [x1, …]` overwrites the very register holding the table base, so the base address is resolved at load time, not deferred.
- **The only environment-level difference from qemu was `$HOME`**: qemu inherits the host's, so the test gives qemu wasm-env's default environment.

**Known gaps**

- **No runtime fallback yet:** an indirect branch to an address discovery missed, or an unimplemented instruction, traps. A runtime translator (or CPU-emulation fallback) is future work.
- **Threads and atomics:** exclusives assume a single thread (`stxr` always succeeds).
- **Wired into the runtime.** `wasm-env run` and the shell's `execve` translate arm64 ELF binaries on first use (`runtime/src/translate.rs`), caching by content hash, and run them as ordinary processes. A recompiled (wasm-native) shell can exec a translated binary; both make the same syscalls. `tests/m2-exec.sh` covers it. Converting whole images is still M4.
- **Not yet run on macOS or Windows.**

## Risks

- **Compile scaling for whole-library translation (addressed).** Translating a whole shared object as one LLVM module exceeded 16 GB in clang. Fixed by chunked compilation: lifted functions split across many small modules compiled in parallel, with all control flow through a binary-search dispatch (no giant switch). The full 1 MB `libc.so` now compiles and links. glibc is larger still; if a single library ever outgrows this, demand-driven translation (only the used symbol closure) is the next lever.

- **Finding all the code.** Static disassembly is undecidable in general (indirect jumps, jump tables, data mixed with code), and worst on x86-64's variable-length instructions. Mitigated by arm64-first, dispatch tables and a runtime fallback.
- **Porting effort.** Recompiling needs each package's build to cross-compile cleanly to the new target. Some will need patches (assembly, `fork`-heavy code, assumptions about the host).
- **fork in WASM.** WASM can't clone a running instance natively. Options: snapshot and copy linear memory (WASIX's approach), or rewrite the control flow so execution can be paused and resumed (asyncify-style). Expect this to be slow at first.
- **Memory ordering.** x86 guarantees a stronger memory ordering between threads (TSO) than WASM does. Rosetta 2 relies on a hardware mode for this that WASM doesn't have. arm64 sidesteps it; multithreaded x86-64 binaries need extra atomics or fences, which will cost performance.
- **SIMD.** SSE, AVX and NEON instructions have to map onto WASM's 128-bit SIMD, or be split into scalar operations.
- **Performance of lifted code.** Lifting loses types and structure, so the generated WASM may be well above the 2–3× target. Measure from M0.
- **Maintaining a wasmtime patch.** Masked addressing lives in a safety-critical file we patch ourselves. Every wasmtime upgrade needs the patch re-applied and re-reviewed until it is upstream or a supported option.
- **Interpreter performance.** The M-1 interpreter loop runs ~3× slower than native in WASM. bash and Python are interpreters, so this hits the first workloads directly. Investigate Cranelift's codegen for switch dispatch, and try a real interpreter (Lua or CPython) early.
- **Numerical performance of translated code.** numpy, pandas and PyTorch spend their time in SIMD kernels and BLAS. NEON maps well onto WASM SIMD, but anything wider (SVE, AVX2/AVX-512 on x86-64) doesn't, and lifted code loses some optimization. Expect numerical code to be the slowest workload; recompiled wheels are the fallback for hot packages.
- **Runtime dynamic linking.** WebAssembly has no standard dynamic linking. Our loader must place modules in one shared memory and function table, apply relocations and resolve versioned glibc symbols, and translate new `.so` files on first load without long pauses.
- **Size of PyTorch.** `libtorch_cpu.so` is hundreds of MB of machine code. Translation time, cache size and Cranelift compile time all scale with it.
- **Code generated inside libraries.** oneDNN (PyTorch) and some BLAS builds generate machine code at runtime. They must be configured to use their non-JIT paths until a runtime translation fallback exists.

## Open questions

- When a binary can't be translated, fall back to emulation automatically (with a warning) or only when asked?
- Lifter: build on remill, rev.ng, or write a purpose-built arm64 → LLVM IR lifter?
- Where do recompiled packages come from? Candidates:
  - **nixpkgs cross-compilation (leading option).** Define `wasm64-linux-musl` as a nixpkgs cross platform and build packages with `pkgsCross`. nixpkgs already cross-builds to musl and to unusual targets, it's reproducible, and it can output OCI images directly (`dockerTools`, nix2container). Nix *builds* the environments; OCI *delivers* them; users never need Nix.
  - Rebuilding Alpine's APKBUILDs with our toolchain (keeps package parity with Alpine images, which helps substitution in the converter).
  - Existing WASM package efforts.
- Should the translation cache be shareable, for example a registry of pre-translated layers?
- Exact host list — is Windows on ARM in scope?
- Ubuntu version and architecture: assumed 24.04 LTS, arm64 first. Is x86-64 Ubuntu needed early (for x86-only wheels)?
- Performance bar for translated numerical code: assumed "works, possibly 2–5× slower" at first, with recompiled wheels as the later speed-up.
- When do we need PyTorch's threads badly enough to pull M6 forward?
- Docker interop depth, for example running converted images via runwasi/containerd.

## Prior art to study

- Rosetta 2 (Apple): x86_64 → arm64, ahead of time with a JIT fallback. The reference model
- Box64, FEX-Emu (x86_64 Linux binaries on arm64), Microsoft Prism (Windows on ARM): binary translators
- remill / McSema, rev.ng, RetDec: lifting machine code to LLVM IR
- WSL1, gVisor: implementing the Linux syscall interface in user space
- WASI, wasmtime: runtime and embedding API
- Wasmer + WASIX: how fork, signals and sockets were done in WASM
- container2wasm, v86, WebVM: emulation-based; candidates for the last-resort path
- Linux kernel ported to WASM (experimental)
- Google Native Client (NaCl): software fault isolation using x86-32 segments, and masking plus guard regions on x86-64. The model for our masked addressing
- Docker + runwasi / containerd WASM shims
- Pyodide: CPython and scientific packages (numpy, pandas, scipy) recompiled for WebAssembly, with build recipes and a wheel platform tag. The model for recompiled wheels
- ZLUDA (CUDA on non-NVIDIA GPUs via PTX translation), chipStar (CUDA/HIP on SPIR-V): GPU research
- FEX-Emu thunks: forwarding selected library calls from translated code to native host libraries
