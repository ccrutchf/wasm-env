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

# Real dynamically-linked musl programs (main + libc.so). musl's dynamic
# linker is libc.so itself; it's translated too and runs first, setting up
# TLS and constructors exactly as under qemu.
for f in hello:tests/c/hello.c control:tests/arm64/control.c memory:tests/arm64/memory.c arith:tests/arm64/arith.c; do
	name=${f%%:*} src=${f#*:}
	$cc -O2 -o "$out/$name-dyn" "$src"
	env -i HOME=/root PATH=/usr/local/bin:/usr/bin:/bin TERM=xterm \
		qemu-aarch64 -L "$AARCH64_MUSL" "$out/$name-dyn" a "b c" >"$out/$name.exp" 2>&1
	exp=$?
	if ! "$root/target/release/wasm-env-translate" translate "$out/$name-dyn" "$out/$name-dyn.wasm" \
		--interp "$AARCH64_MUSL/lib/libc.so" >"$out/$name.tlog" 2>&1; then
		check "$name (libc.so): translation" "ok" "failed"
		continue
	fi
	"$root/target/release/wasm-env" run "$out/$name-dyn.wasm" a "b c" >"$out/$name.act" 2>&1
	act=$?
	sed -i "s|argv\[0\] = .*|argv[0] = PROG|" "$out/$name.exp" "$out/$name.act"
	check "$name (libc.so): exit status" "$exp" "$act"
	check "$name (libc.so): output" "$(cat "$out/$name.exp")" "$(cat "$out/$name.act")"
done

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
