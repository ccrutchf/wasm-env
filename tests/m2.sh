#!/usr/bin/env bash
# M2 acceptance test: differential testing of the arm64 translator.
#
# Each fixture is a C program built as a static arm64 binary. It runs under
# qemu-aarch64 (the reference) and, translated, under wasm-env; stdout and
# exit status must match exactly.
#
#   nix develop -c tests/m2.sh [FIXTURE...]
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
cc=aarch64-unknown-linux-musl-gcc

cargo build -q --release -p wasm-env
cargo build -q --release -p wasm-env-translate --features lift
translate=$root/target/release/wasm-env-translate
wasm_env=$root/target/release/wasm-env

# name:source:flags
fixtures=(
	"min:tests/arm64/min.c:-nostdlib -fno-stack-protector"
	"hello:tests/c/hello.c:"
	"fnptr:tests/arm64/fnptr.c:"
	"arith:tests/arm64/arith.c:"
	"control:tests/arm64/control.c:"
	"memory:tests/arm64/memory.c:"
)

out=$root/build/m2
mkdir -p "$out"
passed=0
failed=0
for f in "${fixtures[@]}"; do
	IFS=: read -r name src flags <<<"$f"
	if [ $# -gt 0 ] && [[ " $* " != *" $name "* ]]; then continue; fi
	# shellcheck disable=SC2086
	$cc -O2 -static $flags -o "$out/$name" "$src"

	set +e
	qemu-aarch64 "$out/$name" a "b c" >"$out/$name.expected" 2>&1
	expected_status=$?
	"$translate" translate "$out/$name" "$out/$name.wasm" 2>"$out/$name.translate.log"
	translate_status=$?
	timeout 60 "$wasm_env" run --root "$out" "$out/$name.wasm" a "b c" >"$out/$name.actual" 2>&1
	actual_status=$?
	set -e

	# argv[0] differs (the qemu path vs the wasm file's name); normalize it.
	sed -i "s|$out/$name|PROG|g; s|argv\[0\] = $name|argv[0] = PROG|" "$out/$name.expected" "$out/$name.actual"
	if [ $translate_status -ne 0 ]; then
		failed=$((failed + 1))
		echo "FAIL $name: translation failed (see $out/$name.translate.log)"
	elif [ $expected_status -ne $actual_status ] || ! cmp -s "$out/$name.expected" "$out/$name.actual"; then
		failed=$((failed + 1))
		echo "FAIL $name: status $actual_status (expected $expected_status)"
		diff "$out/$name.expected" "$out/$name.actual" | head -10 || true
	else
		passed=$((passed + 1))
		echo "ok   $name"
	fi
done

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
