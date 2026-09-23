# M-1: wasm32 vs memory64 benchmarks

Measures how much slower memory64 is than wasm32, on wasmtime (Cranelift) and wasm2c + clang (LLVM), against a native baseline. See the M-1 section of [PLAN.md](../PLAN.md) for the question and decision rule.

## Run

```sh
nix develop                     # from the repo root
make -C bench                   # builds native, wasm32 and wasm64 variants into bench/build/
cargo run --release -p bench-harness -- --runs 9 --out bench/results/<os>-<arch>.json
```

Pass benchmark names to run a subset: `... -- --runs 3 tree sort`.

## What's measured

- `c/*.c` — freestanding benchmarks (no libc). Each exports `setup()` (untimed) and `run()` (timed, returns a checksum).
- `harness/` — Rust harness embedding wasmtime. Each run uses a fresh instance. Configurations:
  - `wasm32`: default (4 GB reservation + guard pages, so most bounds checks are elided)
  - `wasm32-bounds`: no reservation, forcing explicit bounds checks
  - `wasm64`: memory64
  - `wasm64-nospectre`: memory64 with Cranelift's Spectre hardening of bounds checks turned off
  - `wasm64-masked`: memory64 with indexes masked to 38 bits instead of bounds-checked. Needs our patched wasmtime compiler in `vendor/`; `cargo test -p bench-harness` checks its behavior
- `native`: the same C built with clang for the host, driven by `c/native_driver.c`.
- `w2c-wasm32`, `w2c-wasm64`, `w2c-wasm64-masked`: the same `.wasm` files translated to C by `wasm2c` and compiled with clang (LLVM instead of Cranelift). The masked variant inserts `w2c/mask.h` into the generated code and uses `w2c/mem_masked.c` for a fixed 2^38-byte reservation (Linux/macOS only).

Checksums must match across configurations; the harness warns if they don't.

## Reducing noise

- The harness pins itself, and so the benchmark processes it starts, to one CPU core (the last core by default; `--core N` to choose). Pinning isn't supported on macOS; the harness warns.
- Configurations are interleaved: each round runs every configuration once, starting from a different one each round, so background load hits them all evenly.
- Each result shows the median and the interquartile range as a percentage of it. Raw per-run times are in the JSON output.
- The harness warns if the load average is above 1. Run on an idle machine for numbers you want to compare.

## Other hosts

The `.wasm` files are portable: copy `bench/build/wasm32` and `bench/build/wasm64` to the Mac or Windows machine as-is. Only the native builds and the harness need building per host.
