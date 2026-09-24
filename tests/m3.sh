#!/usr/bin/env bash
# M3: dynamic linking. Translates a dynamically-linked arm64 program with
# its shared library into one module (prelinking) and checks it matches
# qemu running the same program through the real dynamic linker.
#
#   nix develop -c tests/m3.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
cc=aarch64-unknown-linux-musl-gcc
cargo build -q --release -p wasm-env -p wasm-env-translate --features lift || exit 1
: "${AARCH64_MUSL:?run inside nix develop}"

out=$root/build/m3
mkdir -p "$out"
$cc -O2 -shared -nostdlib -fno-stack-protector -fPIC -o "$out/dynlib.so" tests/arm64/dynlib.c
$cc -O2 -fPIE -pie -nostdlib -fno-stack-protector -o "$out/dynmain" tests/arm64/dynmain.c -L "$out" "$out/dynlib.so"

passed=0; failed=0
check() { if [ "$2" = "$3" ]; then passed=$((passed+1)); else failed=$((failed+1)); printf 'FAIL %s\n  expected: %q\n  actual:   %q\n' "$1" "$2" "$3"; fi; }

# Reference: qemu with the real ld-musl loading dynlib.so.
qemu-aarch64 -L "$AARCH64_MUSL" -E LD_LIBRARY_PATH="$out" "$out/dynmain" >"$out/exp" 2>&1
exp=$?
# wasm-env: translate main + lib into one module (prelinked), then run.
"$root/target/release/wasm-env-translate" translate "$out/dynmain" "$out/dynmain.wasm" --lib "$out/dynlib.so" \
  >"$out/tlog" 2>&1 || { echo "FAIL: translation failed"; grep -v "traps if" "$out/tlog" | head; exit 1; }
"$root/target/release/wasm-env" run "$out/dynmain.wasm" >"$out/act" 2>&1
act=$?
check "dynamic exit status" "$exp" "$act"
check "dynamic output (cross-object call + data)" "$(cat "$out/exp")" "$(cat "$out/act")"

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
