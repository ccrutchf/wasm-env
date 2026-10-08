# Running wasm-env on other hosts

wasm-env is developed on Linux x86-64, but its goal is to run the same
environments on macOS (arm64) and Windows (x86-64) too (PLAN.md, "Guiding
principle"). This is how to try it on each, from the simplest check to the
largest, and what each step exercises. Run everything from the repository's
root.

## macOS (Apple silicon)

Status: the runtime and translator compile for `aarch64-apple-darwin`
(checked by cross-compiling on Linux); nothing has run on a Mac yet.
Apple silicon's pages are 16 KiB (the guest's are 4 KiB): host mappings
inside the guest's memory (the guest image, `MAP_SHARED` mappings, pages
handed back to be zeroed) follow the host's page size, and the rest of an
image run is copied. If memory misbehaves on a Mac, that is the first
place to look (runtime/src/memory.rs, `host_page`).

1. **Nix, and the development shell.** Install Nix (for example the
   Determinate Systems installer), then `nix develop`. The shell has Rust,
   clang and wasm-ld (translation needs them) and everything else except
   qemu-user and the aarch64 cross compiler, which exist only on Linux:
   the tests that need them (M2, M3's differential checks) can't run on a
   Mac.
2. **Build and unit tests.**
   `cargo build --release` then
   `cargo test --release --manifest-path runtime/Cargo.toml`
   (the runtime's unit tests, including Unix sockets, shared mappings,
   inotify, which is FSEvents on a Mac and untried there, and
   the Wayland compositor, headless) and
   `cargo test --release -p wasm-env-translate --all-features`.
3. **Programs built for wasm-env:** `tests/m0.sh` (the wasm64 toolchain,
   BusyBox) and `tests/m1.sh` (processes, a shell, networking).
4. **An arm64 Linux userland, translated.** `tests/ubuntu-fetch.sh python3`
   fetches Ubuntu 24.04's arm64 base system and Python into build/ubuntu;
   then
   `target/release/wasm-env run --root build/ubuntu /usr/bin/python3.12 -c 'print(1)'`
   translates glibc and Python on first use (minutes), and runs them.
   `tests/ubuntu.sh` runs the whole Ubuntu suite, comparing with qemu where
   it can (on a Mac it can't; those checks will fail or skip).
5. **Graphical programs, in windows of the Mac's own.**
   `tests/ubuntu-fetch.sh wayland-utils weston gtk-3-examples`, then run the
   packages' triggers once (the fetch script only unpacks packages):
   ```
   target/release/wasm-env run --root build/ubuntu /bin/sh -c '
     /usr/lib/aarch64-linux-gnu/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders --update-cache
     update-mime-database /usr/share/mime
     /usr/lib/aarch64-linux-gnu/glib-2.0/glib-compile-schemas /usr/share/glib-2.0/schemas'
   ```
   and then
   `target/release/wasm-env run --wayland --root build/ubuntu /usr/bin/weston-simple-shm`
   (an animated square) and
   `target/release/wasm-env run --wayland --root build/ubuntu --env HOME=/root /usr/bin/gtk3-widget-factory`
   (GTK 3; its libraries take several minutes to translate the first
   time). `--wayland=headless` runs the same without windows.
6. **The GPU service (M9, begun: not yet reachable from the environment).**
   `cargo test --release -p wasm-env-gpu -- --nocapture` compiles OpenCL
   kernels to WGSL and runs them on the Mac's GPU through Metal, checking
   the results against the CPU; it prints the adapter it used. Nothing
   has run on Metal yet.

Where something fails, the command's output (and, for a crash inside a
translated program, `--trace`'s last lines) is what's needed to fix it.

## Windows (x86-64)

Status: the runtime and translator build for `x86_64-pc-windows-gnu`
(cross-compiled on Linux with MinGW's gcc), and the runtime runs under Wine:
its unit tests (90; the few that need a Unix host are left out), M1's 34
checks (processes, a shell, pipes, sockets, networking) and M0's 20;
and it translates arm64 programs itself, with LLVM's Windows build as the
toolchain (M2's `hello`, translated and run on first use).

Ubuntu's own programs run there too, in the Ubuntu environment converted
by the Windows build (the Linux one exported, `wasm-env export build/ubuntu
build/ubuntu-oci.tar`, then `wasm-env convert --lazy --no-precompile` of
that under Wine), checked under Wine: the sound card (`aplay` and
`arecord` byte for byte, `amixer`), the PulseAudio server (`pactl`,
`paplay`, `parecord`, and GStreamer's `pulsesink`, its ORC JIT included),
and the Vulkan driver (tests/arm64/vulkan.c all right, `vkcube`, and
vkQuake 1.36.0 to its 300th frame, through Wine's Vulkan on the Intel GPU
and on lavapipe). What it took: inodes (Windows' metadata has none, and
with every file's 0 glibc's loader took a program's second library for
its first, so no program with two ran), `memfd_create` (a temporary file
deleted on close; libasound uses one), and a JIT's code written through
one mapping of a file and run through another brought over, shared
mappings being copies there (`WASM_ENV_NO_SHARED_MAPPINGS=1` tests that
on Linux). Under Wine, lavapipe needs the settings the GPU service gives
it on Linux (`GALLIUM_OVERRIDE_CPU_CAPS=avx LP_NATIVE_VECTOR_WIDTH=128`,
set outside: a Windows process's environment doesn't reach it), else its
textures are wrong. Under Wine, two of the GPU service's unit tests fail:
an OpenCL program's absolute `#include` within its environment (clang's
overlay on Windows), and OpenCL's doubles in one kernel test.

**Symlinks** in an environment are link files on Windows (making real ones
needs privileges): a small file with the System attribute holding a marker
and the target, which the runtime, the converter and `wasm-env export`
treat as symlinks (runtime/src/fs.rs, `links`). So an environment for
Windows must be unpacked on Windows (`wasm-env convert`), not copied from a
Linux host, whose symlinks Windows can't read.

Not yet:

- **Shared mappings** are copies on Windows (a file view can't be placed
  inside wasmtime's memory reservation): graphical programs' buffers still
  work, since the compositor reads a client's copy (`WASM_ENV_NO_SHARED_MAPPINGS=1`
  tests that on Linux), but two processes don't share memory through a file
  or `MAP_SHARED | MAP_ANONYMOUS` (Python's multiprocessing uses that).
- A native Windows build (MSVC) hasn't been tried.
- **The GPU service** runs on DirectX 12 there, which hasn't been tried
  on Windows itself: under Wine, whose HLSL compiler can't compile the
  shaders wgpu makes for DirectX 12, it's tested through Vulkan instead
  (`WGPU_BACKEND=vulkan tests/cross.sh --unit`).

**The toolchain on Windows.** Translation runs `clang.exe` and
`wasm-ld.exe` from the directory `WASM_ENV_TOOLCHAIN` names (else
`..\lib\wasm-env\toolchain` beside `wasm-env.exe`), laid out as
`bin\clang.exe`, `bin\wasm-ld.exe`, `lib\clang\21\include\` (clang's own
headers) and `lib\libclang_rt.builtins.a` (compiler-rt's builtins for
wasm64: WebAssembly, so the flake's `builtins` package, built anywhere,
serves). The first three come from LLVM's release for Windows
(`clang+llvm-21.1.8-x86_64-pc-windows-msvc.tar.xz`).

On Linux, `tests/cross.sh` checks that everything compiles for macOS and
Windows, with no warnings; `tests/cross.sh --build` also builds the Windows
executables and makes `build/wine-wasm-env`, which runs them under Wine:

```
tests/cross.sh --build
tests/cross.sh --unit        # the runtime's unit tests, under Wine
WASM_ENV_RUNTIME=$PWD/build/wine-wasm-env SKIP_BUILD=1 tests/m1.sh
build/wine-wasm-env run --root build/m2 build/m2/hello.wasm a b
WASM_ENV_TOOLCHAIN='Z:\path\to\win-toolchain' build/wine-wasm-env run --root 'Z:\path\to\root' /hello
```
