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
	# Statically linked (fixed load address).
	"min:tests/arm64/min.c:-static -nostdlib -fno-stack-protector"
	"hello:tests/c/hello.c:-static"
	"fnptr:tests/arm64/fnptr.c:-static"
	"arith:tests/arm64/arith.c:-static"
	"fp:tests/arm64/fp.c:-static"
	"jumptable:tests/arm64/jumptable.S:-static -nostdlib"
	"jumptable2:tests/arm64/jumptable2.S:-static -nostdlib"
	"jumptable3:tests/arm64/jumptable3.S:-static -nostdlib"
	"jumptable4:tests/arm64/jumptable4.S:-static -nostdlib"
	"computedgoto:tests/arm64/computedgoto.c:-static"
	"bti:tests/arm64/bti.S:-static -nostdlib"
	"strrchr:tests/arm64/strrchr.c:-static tests/arm64/glibc_strrchr.S"
	"atomics:tests/arm64/atomics.c:-static -march=armv8.1-a"
	"simd3:tests/arm64/simd3.c:-static -march=armv8.2-a+crc+sha3"
	"simd4:tests/arm64/simd4.c:-static"
	"fpexc:tests/arm64/fpexc.c:-static -lm"
	"flags:tests/arm64/flags.c:-static"
	"calls:tests/arm64/calls.c:-static"
	"control:tests/arm64/control.c:-static"
	"memory:tests/arm64/memory.c:-static"
	# Threads: exclusives (the default), then LSE atomics.
	"threads:tests/arm64/threads.c:-static -pthread"
	"threads-lse:tests/arm64/threads.c:-static -pthread -march=armv8.1-a"
	"signals:tests/arm64/signals.c:-static"
	# C++ exceptions (landing pads found through .eh_frame and the LSDAs).
	"exceptions:tests/arm64/exceptions.cc:-static -pthread"
	# Static PIE (loaded at a chosen base, relocations applied on translate).
	"hello-pie:tests/c/hello.c:-static-pie"
	"control-pie:tests/arm64/control.c:-static-pie"
	"memory-pie:tests/arm64/memory.c:-static-pie"
	# Static PIE with RELR (packed relative relocations), which the program's
	# own startup code applies as *p += base: must not be applied twice.
	"hello-relr:tests/c/hello.c:-static-pie -Wl,-z,pack-relative-relocs"
	"control-relr:tests/arm64/control.c:-static-pie -Wl,-z,pack-relative-relocs"
)

out=$root/build/m2
mkdir -p "$out"
passed=0
failed=0
for f in "${fixtures[@]}"; do
	IFS=: read -r name src flags <<<"$f"
	if [ $# -gt 0 ] && [[ " $* " != *" $name "* ]]; then continue; fi
	# shellcheck disable=SC2086
	compiler=$cc
	[[ $src == *.cc ]] && compiler=aarch64-unknown-linux-musl-g++
	$compiler -O2 $flags -o "$out/$name" "$src"

	set +e
	# Give qemu the same environment wasm-env supplies by default, so only
	# translation differences show up (not inherited host variables).
	env -i HOME=/root PATH=/usr/local/bin:/usr/bin:/bin TERM=xterm \
		"$(command -v qemu-aarch64)" "$out/$name" a "b c" >"$out/$name.expected" 2>&1
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
