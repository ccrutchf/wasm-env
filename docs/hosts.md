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
   (the runtime's unit tests, including Unix sockets, shared mappings and
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

Where something fails, the command's output (and, for a crash inside a
translated program, `--trace`'s last lines) is what's needed to fix it.

## Windows (x86-64)

Status: the runtime and translator build for `x86_64-pc-windows-gnu`
(cross-compiled on Linux with MinGW's gcc), and the runtime runs under Wine:
its unit tests (67; the few that need a Unix host are left out), M1's 34
checks (processes, a shell, pipes, sockets, networking) and 18 of M0's 20;
and it translates arm64 programs itself, with LLVM's Windows build as the
toolchain (M2's `hello`, translated and run on first use). Not yet:

- **Symlinks** in an environment: the runtime follows them itself, reading
  them as the host stores them; under Wine a Linux symlink can't be read,
  so an absolute one reaches the host's file (M0's two failures). How an
  environment's symlinks should be stored on Windows is still open.
- **Shared mappings** (`MAP_SHARED` is a copy on Windows), and with them
  graphical programs' buffers: the compositor reads the files clients draw
  into, which their writes don't reach. Windows' placeholder mappings
  (`MapViewOfFile3`) could place a file view inside the memory reservation.
- A native Windows build (MSVC) hasn't been tried.

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
WASM_ENV_RUNTIME=$PWD/build/wine-wasm-env SKIP_BUILD=1 tests/m1.sh
build/wine-wasm-env run --root build/m2 build/m2/hello.wasm a b
WASM_ENV_TOOLCHAIN='Z:\path\to\win-toolchain' build/wine-wasm-env run --root 'Z:\path\to\root' /hello
```
