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

Status: the translator compiles for `x86_64-pc-windows-gnu`; the runtime
doesn't yet (it uses Unix descriptors and system calls throughout: the
Windows port is still to do, see PLAN.md, M8's portability debt). The
cross-check, on Linux, is how to see what's left:

```
export RUSTUP_HOME=$PWD/build/rustup CARGO_HOME=$PWD/build/cargo-home CARGO_TARGET_DIR=$PWD/build/target-cross
rustup toolchain install 1.98.1 --target x86_64-pc-windows-gnu --target aarch64-apple-darwin
cargo +1.98.1 check --release --target x86_64-pc-windows-gnu --workspace
```

(crates with C code need a C compiler for the target: zig works, as
`CC_x86_64_pc_windows_gnu="zig cc -target x86_64-windows-gnu"`, and for
bad64's bindings `BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu` pointing
at zig's `libc/include/any-windows-any`; the same for
`aarch64-apple-darwin` with zig's `any-darwin-any`.)
