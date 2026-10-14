# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

wasm-env is a Linux environment whose native architecture is WebAssembly
(memory64). Programs are either recompiled to its ABI (a musl port,
`wasm64-linux-musl`) or translated ahead of time from arm64 ELF. Both kinds
run on one runtime, a wasmtime embedding that implements the Linux syscall
interface on Linux, macOS and Windows.

- [PLAN.md](PLAN.md): the design, milestones (M0–M11) and their status.
- [docs/abi.md](docs/abi.md): the ABI, every device interface (GPU, sound,
  Wayland), and what works and what doesn't yet.
- [docs/hosts.md](docs/hosts.md): running on macOS and Windows.

The guiding principle is **portable first, then performance**. Every feature
must work on every host using only what the runtime provides, with a test
that runs anywhere. GUI and GPU don't depend on the host having Wayland or
Vulkan: the runtime has its own compositor (winit) and GPU service (wgpu).
Host-specific fast paths come later, and only as options.

## Commands

Everything runs inside the Nix dev shell, from the repository's root. It
provides Rust, the LLVM toolchain, `WASM_CC`, qemu-user, and the Vulkan and
OpenCL headers.

```
nix develop                     # or: nix develop -c <command>
cargo build --release           # target/release/wasm-env and wasm-env-translate
```

Tests:

```
cargo test --release -p wasm-env -p bench-harness        # runtime units
cargo test -p wasm-env-translate --features lift         # translator units
cargo test --release -p wasm-env-gpu                     # GPU service (host GPU)
WASM_ENV_GPU=software cargo test --release -p wasm-env-gpu device   # on lavapipe
cargo test --release -p wasm-env-gpu <name>              # one test, by name filter
tests/all.sh                                             # the full suite
```

The full suite runs the unit tests, then the milestone scripts:
- `tests/m0.sh` and `tests/m1.sh` (toolchain, BusyBox, processes);
- the differential tests against qemu-aarch64: `tests/m2.sh`, `tests/m3.sh`,
  `tests/alpine.sh`, `tests/python.sh` and `tests/ubuntu.sh`;
- `tests/convert.sh`.

Each script can be run alone. `tests/ubuntu.sh` fetches Ubuntu 24.04 arm64
into `build/ubuntu` the first time (`tests/ubuntu-fetch.sh`). It also builds
the guest drivers (`packages/opencl`, `packages/vulkan`), installs them
there, and runs the Vulkan driver's test (`tests/arm64/vulkan.c`), vkcube and
games.

Other hosts and conformance:
- `tests/cross.sh` checks that macOS and Windows compile with no warnings.
- `tests/cross.sh --build` and `tests/cross.sh --unit` build the Windows
  executables and run the unit tests under Wine.
- `tests/export.sh [--windows]` packs `build/ubuntu`, with freshly built
  drivers, into `export/ubuntu-oci.tar` for another host. With `--windows`
  it also stages the Windows executables and toolchain in `export/windows`.
- `tests/vulkan-cts.sh build` builds Khronos's Vulkan CTS for arm64 into the
  environment.
- `tests/vulkan-cts.sh run [intel|software] 'dEQP-VK.group.*'` runs it. It
  resumes after crashes, and its results go to `build/vk-cts-run/`.

A program in an environment:
`target/release/wasm-env run --root build/ubuntu [--env K=V] /usr/bin/python3.12 ...`.
Add `--trace` (or `WASM_ENV_TRACE=1`) to log syscalls, and
`--wayland=headless` for graphical programs in tests.

### Working rules for this repository

- **Never run `cargo fmt`.** The tree isn't rustfmt-formatted, and it
  rewrites about 60 files. Match the surrounding formatting by hand.
- **Builds and tests are heavy:** translation, the Ubuntu environment and the
  conformance runs. Run one at a time; two at once have run the machine out
  of memory. Wrap long jobs in a memory-capped `systemd-run --user` scope.
- **Commit to `main` only after `tests/all.sh` passes**, and update
  `docs/abi.md` and `PLAN.md` with the change.
- **Vendored crates** (`vendor/`: wasmtime's cranelift, naga, bad64) carry
  local patches marked with a `wasm-env` comment. Each one is listed in the
  root `Cargo.toml`'s `[patch.crates-io]` comments; add any new patch to that
  list.

### GPU selection

The GPU service picks its adapter from three variables:
- `WGPU_BACKEND`: `vulkan`, `metal` or `dx12`;
- `WASM_ENV_GPU`: `software` (lavapipe), or part of an adapter's name, such
  as `intel`;
- `VK_ICD_FILENAMES`: for wgpu's Vulkan backend on Linux.

On this machine:
- **Mesa:** use the pinned build in `build/mesa`
  (`build/mesa/share/vulkan/icd.d/{lvp,intel}_icd.x86_64.json`), not the
  system's `/run/opengl-driver`.
- **Run the suite on lavapipe**, with
  `VK_ICD_FILENAMES=$PWD/build/mesa/share/vulkan/icd.d/lvp_icd.x86_64.json WGPU_BACKEND=vulkan WASM_ENV_GPU=software`.
- **Never use the NVIDIA GPU.**

Debugging:
- `WASM_ENV_DEBUG_GPU=1` prints the adapter, its quirks and wgpu's errors.
- `WASM_ENV_DEBUG_WGSL=1` prints shaders as WGSL.
- `WASM_ENV_DEBUG_WAYLAND=1` reports frames, with their color counts.

## Architecture

**Workspace crates:**
- `runtime/` (`wasm-env`);
- `translator/` (`wasm-env-translate`);
- `gpu/` (`wasm-env-gpu`);
- `bench/harness`.

**Toolchain:** `toolchain/` builds the musl sysroot from an overlay on
pristine musl (`toolchain/build.sh`). `toolchain/bin/wasm64-linux-musl-cc` is
the cross compiler. `packages/*/build.sh` build software with it, or, for
arm64-Linux guest libraries, with clang against `build/ubuntu`.

**Runtime** (`runtime/src/`):
- `main.rs` holds the CLI: `run`, `convert`, `shell`, `export`.
- `syscall.rs` is the Linux syscall layer. Guests call one import,
  `linux.syscall(nr, a0..a5)`.
- `process.rs`, `fork.rs` and `thread.rs` hold the process model.
- `memory.rs` has masked memory64 addressing and `mmap`.
- `fs.rs` has the virtual filesystem; on Windows, symlinks are link files.
- `modules.rs` and `translate.rs` hold the dynamic loader. Translated
  libraries are modules sharing one memory and one table. Each is translated
  on first load and cached by hash in the environment's
  `/var/cache/wasm-env`.
- `convert.rs` and `oci.rs` hold OCI pull, convert and export.
- Devices and services: `wayland/` (the compositor), `audio*.rs` and
  `pulse/` (ALSA's kernel interface and a PulseAudio server), and `gpu.rs`.

**Translator** (`translator/src/`):
- `elf.rs` loads arm64 ELF.
- `discover.rs` finds the code: recursive disassembly, with dispatch tables
  for indirect jumps.
- `lift/` lifts each instruction.
- `wasmgen.rs` generates WebAssembly directly.
- `link.rs` links, and `ehframe.rs` handles C++ unwinding.
- Differential fixtures, run against qemu, are C files in `tests/arm64/`.

**GPU** is split across three places. Guest libraries talk to the runtime
through ioctls on `/dev/wasm-env-gpu`, defined in `runtime/src/gpu.rs`'s
module doc, which documents every request.
- `packages/opencl/icd.c` and `packages/vulkan/icd.c` are guest-side ICDs:
  arm64 Linux `.so` files, loaded by the environment's own ICD loaders.
- `runtime/src/gpu.rs` decodes requests and owns per-session objects.
- `gpu/` holds the wgpu service.
  - **OpenCL:** clang turns OpenCL C into LLVM IR (`ir.rs`). `structure.rs`
    makes its control flow structured, then `wgsl.rs` emits WGSL. `f64.rs`
    supplies software doubles where the device has none, and libclc
    (`packages/libclc`) supplies the builtins.
  - **Vulkan:** the ICD records a command buffer as a byte stream.
    `gpu/src/vulkan.rs` (`decode`, then `submit`/`replay`) replays it onto
    wgpu at `vkQueueSubmit`. Driver and runtime must agree on that wire
    format: change both together, and the op table documented above
    `decode`. Entry points are generated from `vk.xml`
    (`packages/vulkan/entries.awk`, `build.sh`); unimplemented ones are
    stubs.
  - **SPIR-V:** shaders go through rewriting passes in `gpu/src/spirv.rs`
    and `gpu/src/spirv/`, called from `prepare`. These cover what naga can't
    read and what WGSL lacks: combined image samplers split, 1D images made
    2D, interface blocks flattened, std140 matrices, subgroups, integer
    sampling, and so on. Then naga parses the result, and the shader goes to
    wgpu as **naga IR, not WGSL**, so the explicit buffer layouts survive.
    Graphics pipelines are made from a `Recipe`, with variants for dynamic
    state WebGPU bakes into pipelines.
  - **WebGPU limits:** where WebGPU can't express a Vulkan feature, it's
    emulated (snapshots, repacking, twin views, extra draws) or refused
    cleanly as a Vulkan error. A panic in the GPU service must never hang the
    guest: `unpanicked` guards pipelines and submissions. The remaining gaps
    are listed under "Not yet" in `docs/abi.md`.

**Environments:** `build/ubuntu` is the main test environment. `build/` is
untracked scratch space, and holds the pinned Mesa, CTS checkouts and helper
scripts.
