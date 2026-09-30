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
- Graphical applications (Wayland) and the host's GPU (CUDA, Vulkan, OpenCL): see M8, M9 and "GUI and GPU".
- Runs on a native runtime on each host (not in a browser).
- Research environment first; grows workload by workload.
- Learning project first, then possibly a product.

## Non-goals (for now)

- JIT-based runtimes (Node/V8, JVM). They generate machine code at runtime, which ahead-of-time translation can't see; revisit later. Libraries with an internal JIT (oneDNN in PyTorch) must run with it disabled.
- Multithreading for programs built with the wasm-env toolchain (version 0 of the ABI). Translated arm64 programs have real threads (see M5).

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
- **M3 — Translated dynamic linking (in progress).** Slice 1 (done): position-independent executables (ET_DYN). The loader assigns a fixed base, applies relative and defined-symbol relocations at translation time (prelinking), so the 1:1 address model holds. static-PIE fixtures pass differentially vs qemu (`tests/m2.sh`). Slice 2 (done): multi-object prelinking — an executable and its shared libraries are laid out at non-overlapping bases, cross-object symbols and relocations resolved at translation time, and merged into one module. A two-object dynamic program matches qemu (`tests/m3.sh`). Slice 3 (in progress): a real dynamically-linked musl program (main + full `libc.so`). The link step works — symbols resolve, weak-undefined resolve to 0, relocations apply, valid IR is produced — and **chunked compilation now makes it build**: the lifted functions are split across many small modules compiled in parallel, and all control flow routes through a binary-search `dispatch` (in the support code) over a block table, so there is no giant switch. The full 1 MB `libc.so` compiles and links (a 5.6 MB module) with bounded memory. Slice 3b (done): **the real dynamic linker runs, translated.** musl's ld.so is libc.so itself, and for a program that only needs libc it maps nothing, so it can simply be translated along with everything else and started first, as the kernel would (`--interp`; the support code supplies `AT_BASE`, `AT_ENTRY` and `AT_PHDR`). It relocates the prelinked image (idempotently: RELA stores absolute values), sets up TLS, runs constructors and jumps to the program — no reimplementation of ld.so. Dynamic hello/control/memory/arith match qemu byte-for-byte (`tests/m3.sh`); translating a program plus all of libc takes ~3 s on 16 cores. Slice 3c (done): **libraries loaded at run time.** The translator finds the interpreter and DT_NEEDED libraries (recursively) inside a root filesystem the way ld.so will search (`--root`), plus libraries a program only `dlopen`s (`--dlopen`, with their dependencies), and prelinks each at a fixed base. When the translated ld.so opens one of them and maps it, the support code places the mapping at that base (reading the file into place); ld.so relocates it as usual, and every address matches the translation. A program with its own library (constructor, TLS) that `dlopen`s a plugin matches qemu, and the runtime does all of this on `exec`: a native shell runs such a program, the dynamic linker and libraries come from the environment, `/etc/wasm-env/dlopen` declares dlopen'd libraries per program, and cached translations are redone when any input file changes. **Ubuntu 24.04 (glibc):** Ubuntu's own arm64 programs run translated with glibc's real dynamic linker: coreutils, `mawk`, `find`, and **Ubuntu's `python3.12`**, matching qemu (`tests/ubuntu.sh`, which also rebuilds the C fixtures against Ubuntu's glibc with clang and the Ubuntu root as sysroot). glibc needed: placing its 64 KB-aligned library reservations at the translated base; leaving lazy-binding PLT slots unprelinked (glibc adds the load base to them); IFUNC resolvers, PLT0 and BTI landing pads as discovery roots (Ubuntu builds with branch protection, so every indirect-branch target is marked); computed-goto tables; control-flow-aware tracing of table addresses; a real `brk` and single-threaded `futex` in the runtime. Two lifter bugs found through it: post-index SIMD structure loads (`ld1 {v1.16b}, [x2], #16`) didn't advance their base, and `ushl`/`sshl` ignored negative (right-shift) counts. **Python:** Alpine's Python 3.14 runs translated, with its extension modules (OpenSSL `hashlib`, `sqlite3`, `zlib`, `_decimal`, …, declared via `/etc/wasm-env/dlopen`): ~660k blocks, a few minutes to translate the first time, 4 s per cached run, byte-identical to qemu (`tests/python.sh`). Two pieces of infrastructure made this tractable: **incremental translation** (chunks grouped by address range and a content-addressed object cache, so retranslating recompiles only what changed: 51 s → 5 s for Python) and **learning from runs** (a jump to untranslated code is reported through a wasm-env syscall; the runtime records it and the next translation includes it, so translation converges whatever idiom hid the target — the ahead-of-time counterpart of Rosetta's runtime fallback). **Real distribution binaries:** Alpine Linux 3.24's own arm64 BusyBox (dynamically linked against Alpine's musl, RELR relocations, stripped) runs under wasm-env, translated on exec: 24 applets — sed, awk, bc/dc, grep -E, gzip, hashes, date/cal, ash — match qemu byte-for-byte, and a native shell runs pipelines of them (`tests/alpine.sh`). Bugs this found: RELR relocations (decoded for discovery only, since ld.so applies them as `*p += base`, which prelinking must not do first), jump-table entries sign-extended by the dispatching `add`, and jump-table addresses spilled to the stack. **Libraries as modules of their own (done):** each shared library is translated once, for a guest range the runtime keeps for it (from 64 GB up), into a position-independent side module (wasm-ld's `dylink.0` convention), and the executable into a main module with the support code (docs/abi.md, "Libraries"). The runtime loads the dynamic linker as the kernel would; when it maps a library, the support code asks the runtime, which translates the library if needed, places the module's data and function-table slots, instantiates it against the main instance's exports (in every thread's store) and adds it to a directory `dispatch` searches. Guest registers moved from wasm-ld TLS, which side modules can't share, to a per-thread block behind one exported global, read once per region (no measurable cost). So `dlopen` of a library nobody declared works, a program no longer carries every library it might load, and a library's translation serves every program: glibc's `libc.so.6` is 72k blocks, translated once. A libc-heavy loop runs 2.5% more instructions than the monolithic translation (calls into libc cross modules through the same `call_indirect`); `WASM_ENV_MONOLITHIC=1` keeps the old way. Bugs this found: `prlimit64` wrote the current limits over the *new* ones rather than into `old`, so glibc sized thread stacks from garbage (64 GB, which the runtime zeroed page by page: anonymous mappings are now zeroed by handing pages back to the host, `madvise(MADV_DONTNEED)`). Goal: Ubuntu 24.04 arm64's own `python3 -c 'print(1)'`, with its glibc, running translated.
- **M4 — Python wheels (in progress).** `pip install` into the translated Python and import binary wheels: first a small C extension (`markupsafe`), then **numpy** (with bundled OpenBLAS, single-threaded), then **pandas**. Measure against native arm64. **numpy runs:** PyPI's manylinux numpy 2.5.3 wheel (installed by `tests/wheel-fetch.sh`, unmodified, with its bundled OpenBLAS and Highway sort kernels) on Ubuntu's `python3.12` matches qemu on array arithmetic, ufuncs, BLAS (dgemm/sgemm), LAPACK (solve, inv, det, eigvalsh), FFT, random, sorting, strings, float16 and complex (`tests/ubuntu.sh`). ~2M blocks (450 MB of wasm); ~10 min to translate cold, ~1–2 min with a warm object cache, ~40 s for wasmtime to compile it on first run. After that, `python3 -c pass` takes 0.05 s, `import numpy` 0.55 s and the demo 0.6 s (qemu: 1.0, 2.5, 2.25 s); a pure-Python loop benchmark runs ~2.5× faster than qemu, but perhaps 8× slower than native. What made runs fast: `dispatch` finds the 4 KB region through a small direct-mapped cache (it binary-searched all 2M block addresses, 62% of `import numpy`); compiled modules are cached uncompressed and mapped from the file (wasmtime's cache re-hashed the 450 MB .wasm and decompressed its entry on every run); the guest image is part of the module's data, mapped copy-on-write. **NEON as vector operations:** vector FP arithmetic, fused multiply-add (by element too), min/max and interleaved loads/stores are lifted as LLVM vector operations (`<4 x float>`, `<2 x double>`, shuffles), and lane access uses `extractelement`/`insertelement` rather than i128 shifts (wasm has no i128), so LLVM re-vectorizes lane-wise code; FP exception checks batch every 16 instructions. numpy on a 300×300 matrix: `dgemm` 136 → 27 ms, `sgemm` 50 → 9.5 ms, `solve` 42 → 15 ms; `exp` over 1M doubles 122 → 71 ms; OpenBLAS alone translates 37% faster. LLVM 21's WebAssembly backend all but hangs on half-precision vector comparisons, which its vectorizer forms from lane code: half is compared as float behind `llvm.arithmetic.fence`, and a unit that takes over 10 minutes to compile is recompiled without the SLP vectorizer. **Registers across blocks:** keeping guest registers in LLVM locals across a region (copied in on entry, out at every exit) made CPython 30% slower and its module 30% bigger (52 → 67 MB): nearly every region of an interpreter has a loop and many exits, so the copies cost more than they save. Instead, TBAA metadata tells LLVM that guest memory never aliases the register globals, so it forwards and hoists register values where that pays: no size cost, OpenBLAS a little faster and smaller, CPython unchanged. **Flags and dispatch:** flag liveness over the whole program (calls and returns don't carry NZCV, per AAPCS64) drops the flag stores, and with them the computations, where no successor reads them; carry and overflow are computed at the operands' width (wasm has no i128); indirect branches probe the dispatch cache inline. CPython: 12% fewer instructions, module 52 → 45 MB, compiles 20% faster. **Tried for Python and dropped:** mapping guest calls and returns onto wasm calls (branch misses fell by a third but were under 1% of cycles; code 22% bigger), 16 KB and 64 KB regions (64 KB: 20% more instructions and 10× the compile time, so function-sized regions would hurt), and in-region computed jumps (3% fewer cycles for 58% more compile time). A pure-Python mix ran about 8.5× slower than native CPython (6.6× the instructions at lower IPC), 3× faster than qemu. **Offsets into the access:** a register access was `lea off(state)`, then the mask, then the load, because the lifter's address arithmetic could wrap, so LLVM couldn't fold constant offsets into the WebAssembly access's offset immediate; with `inbounds nuw` on the state GEPs and `nuw` on positive guest offsets (`[xN, #imm]`, pairs, structure loads), every access off one base shares its masked index and a register access is one `mov`: 18% fewer instructions and ~10% fewer cycles on the same mix, now 4–12× native (x86-64 CPython: nbody 4×, strings 6.6×, classes 7.2×, fib 12×). The profile's remaining overheads: each block loads the registers it reads and stores those it writes (the state is memory), and every region entry pays a prologue (stack check, callee-saved registers) and the entry switch: ~13% of the hottest regions' samples. **Registers in locals, with liveness (tried, dropped):** each region function kept guest registers in locals (allocas, so SSA), each entry loading only the registers live into it and each exit storing only those possibly written on the way (forward and backward analyses over the region; a system call stores and reloads them all). It passes the differential tests, but CPython ran 3% more instructions: nearly every block of the interpreter's regions is an entry (an opcode handler) and ends in an exit (the computed-goto dispatch), and an exit must store every register the region may have written on any path, which forces entries to load them too (state loads 378 → 700, stores 525 → 717 in the hottest region). Keeping dispatch inside the function (64 KB regions, so `_PyEval_EvalFrameDefault` is one function, and computed gotos within a region through the function's own switch) was 3× slower and took 16× as long to translate: phis for 64 registers at hundreds of switch targets swamp register allocation (64 KB regions alone: 13% more instructions). What would help instead is fewer registers crossing region boundaries, e.g. passing the few hot ones as arguments of the tail calls. What remains is structural: guest state in memory at every region entry and exit, and a masked address on every guest memory access. **Entries and exits:** Cranelift's `tail` calling convention, which Wasmtime compiles every wasm function with, kept the System V callee-saved registers, so a region function saved five registers on entry and restored them at each of its exits (885 of the hottest region's 8,276 instructions), although region functions only tail-call one another; it now has none (vendor/cranelift-codegen). And flags are no longer taken as live across an indirect jump (compiled code sets them again after a computed jump), which dropped the flag computations ahead of every opcode dispatch. Together: 12% fewer instructions and 6% fewer cycles on the pure-Python mix, the hottest region 20% smaller. What remains is mostly instruction-cache misses (0.6G, against native's 0.002G): translated code is 7 to 8 times the size of the arm64 it comes from. A masked memory's base is now loaded once, at function entry: it was pure to Cranelift's e-graph, so it was loaded again (three dependent loads from the spilled `vmctx`) in every arm of a region's entry switch; 3% fewer instructions, 5% fewer cycles. **Tried and dropped:** one shared exit per target in each region function (a phi for the address, then the tail call). LLVM's late tail duplication copies it back into every predecessor; without that pass the hottest region has 79 tail calls instead of 154 and chunks are 2.4% smaller, but the run takes slightly more instructions and misses the instruction cache no less. Remaining costs: `dispatch` is still ~15% of a Python loop (returns and CPython's computed gotos, whose targets span many regions). Next: keep registers in locals across a region, lazy flags. Needed: region lifting (one wasm function per 4 KB of code, direct branches inside it, so a module stays under wasm's 1M-function limit), a synthetic `/proc/cpuinfo` and `/sys/devices/system/cpu/*` (OpenBLAS reads them unchecked), `AT_HWCAP` limited to what translates (FP, ASIMD, CRC32, atomics, LRCPC: no SVE, so libraries pick NEON kernels), and many more NEON instructions. Bugs found: lane-form structure loads/stores (`ld2 {v0.s, v1.s}[1]`) treated as whole-register, and vector `mvn` computing all-ones. The qemu reference runs without SVE so OpenBLAS picks the same kernels. **pandas runs:** pandas 3.0.6 (with python-dateutil, tzdata) matches qemu on `tests/python/pandas_demo.py`, 2.0 s against qemu's 5.6 s. **pandas' own test suite** (shipped in the wheel; pytest and hypothesis installed from wheels too) runs under wasm-env: across 18 test directories (not slow/network/db), 133,751 tests pass; every failure also fails under qemu (test data files the wheel doesn't ship), except the ones that led to fixes. What it found: `fmin`/`fmax` returning the other operand for NaN (arm64 propagates NaN; now `llvm.minimum`/`maximum`, wasm's own min/max); translated programs couldn't create processes (`subprocess` → ENOSYS; now vfork, posix_spawn and system work: docs/abi.md, "Processes"); `uname` reported `wasm64` to arm64 programs (pandas and pip decide by it; now `aarch64`); multiply-add was unfused, which broke glibc's `pow`/`exp` error compensation (several ulps off; now fused via relaxed SIMD's `madd`, which the runtime runs deterministically); FPSR exception flags weren't modelled (now invalid, divide-by-zero and overflow, which numpy reports by default, are raised by FP arithmetic and conversions: each result costs a comparison, and each instruction one branch to a support routine that works out the exact flags, taken only when a result is NaN or infinite; underflow and inexact aren't modelled). Resolving per instruction matters: deferring to the end of a block kept every lane's values live across OpenBLAS's huge unrolled kernels, and clang took over an hour on single 30 MB chunks. Those chunks (NEON lifted lane by lane into scalar IR) are the main cost of translating numpy: lifting NEON to LLVM vector operations would shrink them a lot. Also, library bases are now kept from one translation to the next (`--layout`), so adding a library (a new `dlopen` directory) recompiles only its code: 74 s instead of 13 min.
- **M5 — Ubuntu images (working).** `wasm-env convert ubuntu:24.04` (arm64): pre-translate the image, keep dpkg metadata, and `wasm-env run` gives a shell in it. **Done:** `wasm-env convert IMAGE DIR` pulls an OCI image from its registry (anonymous bearer tokens, the linux/arm64 manifest, layers checked by digest and cached) or takes a root filesystem tarball or directory, unpacks the layers with their whiteouts, and translates every arm64 program and library in it (723 for ubuntu:24.04, 8 at a time: 226 s) into the image's own cache (`/var/cache/wasm-env`, with its own library address registry), so the environment carries its translations. `wasm-env shell DIR` is a login shell in it. What it took: **fork** for translated programs (a copy of the process: the guest's state at a syscall is all in memory, so a child is new instances on a copy of the in-use pages, resumed after the `clone`; a few ms each), which Ubuntu's `sh` and `bash` need for pipelines and subshells; terminal ioctls passed to the host terminal and a virtual foreground process group (readline, job control, Ctrl-C to the guest's foreground group); `statx`, `statfs`, `mincore`, xattrs, `/proc/self/mountinfo`; credentials (apt downloads as `_apt`); record locks and `flock` as the host's open-file-description locks; and small fixes found on the way (32-bit `AT_FDCWD`, pipe ends' access modes for `fdopen`, distinct inode numbers for `/dev/null` and stdio, which GNU grep compares, `fsync` on directories, `utimensat` on dangling symlinks). In the converted image, **`apt-get update` and `apt-get install python3` work** (58 s, maintainer scripts included, Python translated on first use), and installed programs run (`tests/convert.sh`). **Portable, shareable images:** translations are keyed by content and the translator's version (a hash of its sources, built into the runtime), not the binary's timestamp, and libraries in an image are registered by their path in it, so a converted image moves between directories and machines without translating again. `wasm-env export DIR OUT.tar` writes it as an OCI image layout (platform wasi/wasm, one layer with the translations: 139 MB for Ubuntu), and `convert` reads OCI layouts back (or `docker save` output), translating nothing again. **Without the dev shell:** `nix profile install` (flake `packages.default`) installs wasm-env with its toolchain (clang, wasm-ld and compiler-rt builtins for wasm64, a Nix derivation of their own) in `lib/wasm-env/toolchain`, which the translator finds (or `$WASM_ENV_TOOLCHAIN`), so programs translate on first use anywhere Nix runs. Remaining: a toolchain bundle for hosts without Nix (macOS, Windows); pushing images to registries. Networking (sockets, DNS) so `pip` can reach PyPI. **Networking works (done early):** host sockets for IPv4/IPv6 TCP and UDP, poll/select across sockets, pipes and files, and the host's resolver configuration when the image has none (docs/abi.md, "Networking"). Ubuntu's translated Python resolves names through glibc, speaks HTTPS through Ubuntu's OpenSSL with its CA certificates, and **`pip install` works**: `pip install requests` takes 9 s. A package with an extension module (requests needs charset-normalizer's) is picked up by the next run of Python, which retranslates incrementally (~2 min here) because the translation cache now fingerprints `dlopen` directories by the shared objects under them (so bytecode caches and pure-Python installs don't count). **Threads (done early):** translated programs run real threads in parallel (docs/abi.md, "Threads"). Each guest thread is its own instance of the module on a shared WebAssembly memory, in its own wasmtime store and host thread; guest registers, the exclusive monitor and the dispatch cache are thread-local (wasm-ld's TLS, so a region reads `__tls_base` once), and `clone(CLONE_THREAD)` starts a new instance from a frame of the parent's registers. Acquire/release accesses, LSE atomics and barriers are WebAssembly atomics; a store-exclusive is a compare-and-swap against the value the load-exclusive read (as QEMU does). futex is real, and exit, exit_group, exec and faults end threads as Linux does. Shared memories can't use wasmtime's copy-on-write data, so the guest image moved into a custom section of page-aligned runs that the runtime maps privately from the .wasm itself. A pitfall: small loops that only load (a thread polling a `volatile` flag) had their load hoisted by LLVM, which knows guest memory and registers don't alias; their loads are now volatile. musl and glibc pthreads (mutexes, condition variables, barriers, semaphores, TLS) match qemu, Python's `threading` and thread pools work, pip draws its progress bar, and OpenBLAS runs `dgemm` (1000×1000) in 1034 ms on one thread, 345 ms on four and 299 ms on 16. Single-threaded speed is unchanged (the Python mix is within noise of before). **Extension modules load without retranslating Python** (libraries as modules, M3): `pip install markupsafe` then importing its C extension takes 0.38 s the first time (translating just that `.so`), 0.12 s after; `/etc/wasm-env/dlopen` is no longer needed. Remaining: a thread stuck in a loop with no syscalls outlives its process's exit.
- **M6 — PyTorch (CPU).** Threads (done for translated programs, see M5), C++ exceptions through translated code, and code generated at run time (oneDNN's JIT). **C++ exceptions work:** the unwinder runs translated and needs nothing special (the guest's stack and registers are real), except that landing pads (catch blocks, cleanups) are reached only through exception tables, so discovery reads `.eh_frame` and each function's LSDA and adds them as roots (translator/src/ehframe.rs); also, hint-space instructions the decoder doesn't know (GCS's `chkfeat`, in libgcc's unwinder) decode as the NOPs they are on CPUs without the feature. A C++ fixture (throws across frames, destructors, base-class catches, rethrow, library exceptions, `exception_ptr` across threads) matches qemu. **Signal handlers run** in translated programs (docs/abi.md, "Signals"): the support code builds the arm64 `rt_sigframe` and handles `rt_sigreturn` (and returns through a dispatch address for glibc, which relies on the vDSO's trampoline); masks, `sigaltstack`, `sigsuspend`, `setitimer` and SIGCHLD work, and Python's `signal` module and `KeyboardInterrupt` match qemu. **PyTorch runs:** with PyTorch's CPU wheel (2.9.1+cpu), `import torch`, a small MLP's and CNN's forward passes, training steps (autograd, SGD) and linear algebra match qemu exactly (tests/python/torch_demo.py, in tests/ubuntu.sh). At first oneDNN was disabled (`torch.backends.mkldnn.enabled = False`), since it generates machine code at run time. **Code generated at run time is translated** now (docs/abi.md, "Code generated at run time"): the runtime records memory made executable, and the first jump into it translates the mapping (the translator's `--raw` input) and loads it as a module, cached by contents, so later runs reuse it; unmapping it or making it writable again retires it. With oneDNN on, the demo matches qemu too: its six Xbyak buffers (256 KB each) translate once, and a warm run takes about the same time as with ATen's kernels (17 s against 16 s). **LuaJIT** (Ubuntu's, its JIT compiling traces as it runs) matches qemu too (tests/lua/jit_demo.lua, in tests/ubuntu.sh). It needed: `mmap` address hints honoured (its code must lie within reach of direct branches from its VM, so it probes random nearby addresses, which the break's reserve now provides); translations covering only the pages their code occupies, one module per page, so a new trace costs a translation of its pages rather than of every trace so far; retiring only code whose bytes changed when memory is made executable again (LuaJIT makes the whole area writable for each trace); `-O1` for generated code (half the compile time of `-O2`, nearly the same code); and reusing retired modules' directory slots. Its interpreter also reaches its bytecode handlers through a table built at run time, one missed target per run: around a missed target, discovery now takes the instruction after each unconditional branch or return as a block start too, so a run reveals a whole page of handlers, and the missed addresses are kept by the binary alone, so they survive a rebuilt translator. **Missed blocks are translated at run time too** (docs/abi.md, "Blocks a translation missed"): the run translates the page again with the new address (the translator's `--page`), and the patch takes over the page's entry in its owner's region table, so a program no longer stops at each block its translation missed; the address is still recorded, so the next translation has it. LuaJIT's first run from a fresh translation now completes, patching 9 pages (15 s: each patch runs discovery over the whole binary), where it took 9 runs before. The demo takes 10–25 s against qemu's 10 s (LuaJIT places its code at random, so each run translates its traces afresh, 40–70 of them at 0.15–0.3 s each); with its JIT off, the translated interpreter is about as fast as qemu's (81 s against 74 s). (Code changed in memory that stays writable and executable, without `mprotect`, is now noticed through the cache maintenance that must precede running it: see M7.) A fixture (`tests/arm64/jit.c`: kernels entered separately and mid-kernel, calls back into compiled code, rewriting, remapping) matches qemu. Getting there needed translation in bounded memory (instructions decode on demand and the lift streams chunks to the compiler, taking the translator for libtorch_python from 3.9 GB to 165 MB), reservations at a nonzero address hint, and jump tables whose bound is checked on the register the index was copied from. The first translation takes about two hours and peaks at 11 GB (wasmtime compiling libtorch_cpu). After that, `import torch` takes 10 s and the demo 15 s (qemu: 23 and 33 s). Those were 50 and 57 s at first; profiling found two causes, neither in the translated code. The virtual counter (`cntvct_el0`) read as a constant 0, so OpenBLAS's idle threads, which spin until 2^28 ticks have passed before sleeping, never slept: one per core spun whenever BLAS was idle, taking the cores from the main thread (586 s of CPU for the demo). It now reads CLOCK_MONOTONIC in nanoseconds, with `cntfrq_el0` 1 GHz. And each thread instantiates every library in its own store, which found each `GOT.func` import's table slot by searching the list of them, quadratic in their number, which is large for libtorch. Also, a library's content hash (its cache key) is remembered against its size, mtime and inode instead of reading and hashing hundreds of MB on every load.
- **M7 — Node.js (working).** Ubuntu's Node.js 18 (V8 in `libnode.so`, 44 MB: 1.46M blocks, about 3 minutes to translate) runs `tests/node/demo.js` identically to qemu, with V8's interpreter only (`--jitless`) and with its JIT: the language and its library, zlib and OpenSSL, the file system, a child process, a worker thread (a second isolate), an HTTP round trip over loopback (`tests/ubuntu.sh`). It needed: libuv's event loop, so `eventfd2`, `epoll_*` (over the runtime's poll, level-triggered) and `socketpair` (a child's stdio), with an M2 fixture of its own (`tests/arm64/events.c`); a translator bug (`smnegl`/`umnegl` read a fourth operand); and cheap patches of missed pages: V8's builtins are entered through tables built at run time, so a run patches pages as it reaches them, 263 on the first run. A patch now reads the blocks the full translation found (its `.leaders`, written with `--leaders-out`) and discovers only its page again, and the lifter lifts only that page (every block an entry, flags live), which took a patch from 6 s to 0.3 s; and a large module (over 8 MB) isn't dropped for retranslation when a target was missed there (that takes minutes): its patches come from the cache. The demo takes 25 s jitless against qemu's 7.6 s. By CPU time the jitless demo is about 2.2× faster than qemu (3.5–3.9 s user against 8.3 s); wall times on a loaded machine said otherwise. With the JIT on it first took about 4 minutes: each new V8 code object retranslated about 24 pages of its neighbours (median 318 blocks), because discovery followed branches through V8's densely packed code space, starting from the mapping's base (taken as an entry) on every translation, and merged every module on those pages. Generated code is now discovered only from its roots, within their pages, without the whole-mapping scans: 1.7 pages per translation (median 35 blocks), half as many translations, and a warm run takes 69 s (qemu: 6 s). What was left was the external translator's fixed cost, about 0.18 s a translation (clang and wasm-ld 0.09 s, a process, wasmtime's compile). **A direct compiler** (translator/src/wasmgen.rs) now compiles the lifter's IR for generated code and page patches straight to WebAssembly: the IR is a small, regular subset of LLVM's (scalar integers and floats, i128 registers as v128, loads and stores, compares, branches, a switch, calls, tail calls, a dozen intrinsics), each basic block a case of a loop around a `br_table`, phis copies on edges; the module is the side module wasm-ld makes, with `__wasm_apply_data_relocs` writing the region table. Anything else (vector operations, fma, saturating arithmetic) goes to clang. A translation now takes 3–30 ms instead of 0.1–0.5 s: LuaJIT's demo 10 s (qemu 10 s; every translation direct), Node with V8's JIT 23 s warm (qemu 6 s; 281 translations, 7 s translating and 12 s loading them, mostly Cranelift compiling each). Generated code is now translated in the runtime's own process (the translator is also a library, `translate_generated`), about a millisecond each, falling back to the translator's process and clang for what the direct compiler doesn't handle (2 of 280 for Node): Node with the JIT 21–24 s, LuaJIT 9 s. Cranelift compiling each module was then the main cost (11–13 s for Node), its register allocator struggling with the `br_table` loop. **Structured control flow** (Ramsey's "Beyond Relooper": reverse postorder and dominators; a `loop` at each target of a back edge, a `block` in its dominator for each block reached by two or more forward branches, everything else inline where it's branched to) now shapes 99.5% of functions (region entries rarely make control flow irreducible; those keep the `br_table` loop), and modules are 1.6 times clang's size: Node with the JIT 18 s warm, loading 6 s. Translating was then the larger part (7 s): each new code object translated its page's other code again (one module per page), 117 blocks a translation on average. **Chaining** (docs/abi.md, "Chaining") ends that: a translation covers only the new code, and an address its region function has no block for goes on to the older function of that region on the page, through a table slot the module imports; `dispatch` takes the newest module with the region. Node with V8's JIT translates 6–7 thousand blocks instead of 33 thousand: 7–8 s warm against qemu's 6 s (and 25 s jitless); LuaJIT's demo 6 s against qemu's 10 s. Constant address arithmetic is folded into loads' and stores' offsets and single-use values stay on the stack, which brought its modules from 3.2 to 1.8 times clang's size (Cranelift's time follows size). Wide vectors (a structure load or store, `ld3`'s `<24 x i8>`) are kept in consecutive v128 locals and split by `i8x16.shuffle`, so the direct compiler now takes every one of Node's translations (it gave 2 to clang). Next: translating in-process (no process per translation), and structured control flow instead of the `br_table` loop, which is slow for Cranelift to compile and for the code to run.
- **M8 — GUI (Wayland).** First the plumbing GUI and GPU share: Unix sockets with paths, descriptor passing (`SCM_RIGHTS`), `memfd_create` and shared mappings of host memory inside the reservation, each with a fixture. Then Wayland clients on a Linux host, forwarded to its compositor: `weston-simple-shm`, then GTK and Qt applications in software. Then GPU buffers (dma-buf) once M9 forwards Vulkan, and a compositor of our own for macOS and Windows hosts. See "GUI and GPU".
- **M9 — GPU (CUDA, Vulkan, OpenCL).** The APIs forwarded to the host's drivers through guest stub libraries (API remoting): CUDA first (`nvidia-smi`, a CUDA sample, then `torch.cuda` from PyTorch's arm64 CUDA wheel on the host's GPU), then OpenCL (`clinfo`, pyopencl), then Vulkan, generated from `vk.xml` (`vulkaninfo`, a compute sample, presenting to Wayland). Later, translation for hosts without an API (PTX to SPIR-V on Vulkan) and a WebGPU subset for hosts with nothing else. See "GUI and GPU".
- **Later.** Native recompiled userland and recompiled wheels for hot packages (nixpkgs cross-compilation; Pyodide recipes); x86-64 translation for x86-only images; runtime translation fallback; CPU emulation fallback.

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

## GUI and GPU

Both rest on the same runtime plumbing, so it comes first:

- **Unix sockets with paths and descriptor passing.** `socket(AF_UNIX)`, `bind`, `connect` and `accept` on host sockets, and `SCM_RIGHTS` in `sendmsg`/`recvmsg`, translating guest descriptors to host ones and back. Wayland passes every buffer and keymap this way; D-Bus and X11 need the same.
- **Shared mappings of host memory.** `memfd_create`, and `mmap(MAP_SHARED)` of a host file or memfd placed inside the process's memory reservation (a real shared host mapping, not a copy), so another process, a compositor or a GPU driver sees the guest's writes. Memory a host driver hands out must land in the reservation too, or the guest can't reach it (below).
- **Calls from the host into the guest.** A host library's callbacks (a CUDA stream callback, a Vulkan debug messenger) run guest code, on a guest thread.

A fixture for each, differential against qemu (an fd and shared memory passed over a socket pair), before any GUI or GPU work.

### GUI: Wayland

1. **Forwarding to the host's compositor (Linux hosts).** The runtime hands the guest the host's `$WAYLAND_DISPLAY` socket. The protocol needs no parsing: with descriptor passing and shared mappings, the host compositor reads the guest's `wl_shm` buffers directly. First `weston-simple-shm`, then a GTK and a Qt application rendering in software.
2. **GPU buffers** (`linux-dmabuf`, EGL and Vulkan window-system integration), once the GPU work below forwards Vulkan: the guest's buffers are the host driver's, exported as dma-bufs to the host compositor.
3. **A compositor of our own (macOS and Windows hosts),** in the runtime, probably with Smithay, each guest toplevel a host window. Needed wherever the host has no Wayland.

Software rendering is fine for toolkit applications; OpenGL through translated llvmpipe (which generates code at run time) would be slow, so GL applications wait for the GPU.

### GPU: CUDA, Vulkan and OpenCL

The base layer is the host's own GPU stack, not wgpu: CUDA's pointer model and Vulkan's feature set don't fit WebGPU (no pointers in buffers, no 64-bit types, limited atomics).

**Tier 1: forwarding the APIs to the host's drivers** (API remoting, like FEX-Emu's thunks). The guest gets stub libraries (`libcuda.so.1`, a Vulkan ICD, an OpenCL ICD) whose functions call into the runtime, which calls the host's real library.

- GPU code doesn't depend on the CPU, so kernels run unmodified: the cubins in an arm64 PyTorch CUDA wheel, cuBLAS and cuDNN run on the host's GPU as they are, and the closed libraries stay guest code calling the driver API. Nothing needs reimplementing.
- A guest pointer is a host pointer plus the reservation's base, so arguments and structures translate cheaply. Vulkan's API is large but machine-readable (`vk.xml`): its forwarding is generated.
- Memory the driver maps for the host (`vkMapMemory`, `cuMemHostAlloc`, managed memory) must be inside the reservation: through Vulkan's external host memory (importing the guest's own pages), CUDA's virtual memory management (reserving and mapping at an address we choose), or an exported descriptor mapped at a fixed address.
- **The main risk:** `libcudart` asks the driver for undocumented function tables (`cuGetExportTable`), which ZLUDA had to reverse-engineer. Their entries are forwarded through generic trampolines, which works as long as they take integer and pointer arguments.

**Tier 2: translation where the host lacks an API** (CUDA on AMD, Intel or Apple GPUs): PTX translated to SPIR-V on Vulkan, ZLUDA-style, with buffer device addresses as CUDA's pointers. OpenCL can come from the guest's own Mesa, unmodified (rusticl on zink), over forwarded Vulkan. No Mesa fork: a WebGPU driver for Mesa's Gallium would be a large fork for less than this gives.

**Tier 3: WebGPU,** only for a host that has nothing else (a browser, should wasm-env ever run in one), and only a compute subset (OpenCL-like).

Testing: qemu has no GPU, so the references are the same programs on the host natively, or CPU implementations (pocl, PyTorch's CPU results within tolerance).

## M2 notes

**How translation works** (see also `docs/abi.md` and the module docs)

- **Discovery** (`translator/src/discover.rs`) decodes every 4-byte word in the executable sections, then finds basic blocks by following control flow from roots: the entry point, function symbols, and addresses formed as code pointers (ADR/ADRP+ADD, or stored in data). It recognizes the GCC/LLVM **jump-table** idiom (`adrp`+`add` table base, `ldr{b,h,sw}` index, `adr` label, `add …, ext #2`, `br`) and reads the offset table, so `switch` targets are found without executing anything.
- **Lifting** (`translator/src/lift/`) turns each block into an LLVM function. Guest registers live in globals (`@R`, `@F` flags, `@V`, `@TP`), cached as SSA values within a block and flushed at its end. Guest memory *is* the WebAssembly memory: guest address A is WebAssembly address A, so the masked-memory64 runtime bounds it for free. Blocks pass control by **guaranteed tail call** (`musttail` → `return_call`), so the host stack never grows however the guest branches, calls or returns; indirect branches (BR/BLR/RET) go through a `@dispatch` switch over every block start.
- **Linking** (`translator/src/link.rs`) compiles the IR and the support code (`translator/support/runtime.c`) for wasm64 with clang, and links them with wasm-ld plus the compiler-rt builtins (`__multi3` etc., which LLVM emits for 128-bit arithmetic and divide-by-constant). The support code maps the guest image at its original addresses, builds the Linux initial stack (adding `AT_PHDR`/`AT_ENTRY`), and forwards `svc` to the `linux.syscall` import — so translated and recompiled programs make identical syscalls.

**Coverage.** Integer arithmetic/logic/shifts/bitfields/conditionals, all load/store forms (including pairs, exclusives as single-threaded, and `ld1`/`st1`), scalar floating point (arithmetic, compare, convert, fma, select, every `frint*` rounding mode and rounding-mode conversions), and a NEON subset (element-wise integer and FP arithmetic and compares, `dup`/`movi`/`mvni` including `MSL`, `cnt`, reductions, pairwise ops, shifts, widening, permutes, `tbl`/`tbx`, bitwise select, reversals, narrowing, element moves). **Every instruction in musl's `libc.so` translates** (`tests/arm64/fp.c` covers the libm-heavy ones). Round-half-away (`frinta`, `fcvtas`) has no WebAssembly instruction and `llvm.round` would need libm, so it is computed exactly as truncate-then-step. Cache-maintenance ops are no-ops. Anything unimplemented lifts to a trap that names the instruction and address, so gaps surface at runtime rather than as wrong answers.

**Findings**

- **bad64 names x29/x30 `fp`/`lr`**; the register decoder handles those aliases.
- **Divide-by-constant introduces 128-bit multiply.** LLVM strength-reduces `x / 10` to a 64×64→128 multiply, so translated programs need compiler-rt's `__multi3`; the sysroot's builtins archive is linked in.
- **Vector results are always full i128.** `vbuild` zero-fills unused high lanes (the arm64 rule that writing a 64-bit vector result clears the upper half), so results are written back as 128 bits.
- **Jump tables reuse the base register.** `ldrh w1, [x1, …]` overwrites the very register holding the table base, so the base address is resolved at load time, not deferred.
- **The only environment-level difference from qemu was `$HOME`**: qemu inherits the host's, so the test gives qemu wasm-env's default environment.

**Known gaps**

- **No runtime fallback yet:** an indirect branch to an address discovery missed, or an unimplemented instruction, traps. A runtime translator (or CPU-emulation fallback) is future work.
- **Threads and atomics:** WebAssembly atomics are sequentially consistent, stronger (and on some hosts slower) than the acquire/release arm64 code asks for; a store-exclusive is a compare-and-swap, so a write of the same value between the load-exclusive and it goes unnoticed (as in QEMU).
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
- Code generated at run time: translated in-process in about a millisecond, chained rather than retranslated, faster than qemu for Node with V8's JIT (4.4 s against 6 s, with lane-level vectors in the direct compiler); code rewritten in pages that stay writable and executable is noticed through the cache maintenance the architecture requires (`ic ivau`, then `isb`). And LuaJIT's interpreter, which dispatches each bytecode with a computed branch, runs no faster translated than under qemu: dispatch through the region functions costs more than qemu's chained blocks there.
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
- ZLUDA (CUDA on non-NVIDIA GPUs via PTX translation), chipStar (CUDA/HIP on SPIR-V): GPU translation (M9, tier 2)
- rCUDA (CUDA API remoting), Mesa venus with virglrenderer (Vulkan forwarded out of a VM): GPU forwarding (M9)
- waypipe and ChromeOS Sommelier (Wayland forwarded across a boundary, with buffers), Smithay (Rust compositor library): GUI (M8)
- FEX-Emu thunks: forwarding selected library calls from translated code to native host libraries: the model for GPU forwarding (M9)
