#!/usr/bin/env bash
# Runs every test suite: unit tests, then the milestone acceptance tests.
#
#   nix develop -c tests/all.sh
#
# M2 (the arm64 translator) is expected to fail until
# translator/src/lift/insn.rs is complete.
set -uo pipefail
cd "$(dirname "$0")/.."

status=0
step() {
	echo "== $1"
	shift
	if ! "$@"; then
		echo "!! failed: $*"
		status=1
	fi
}

step "unit tests (runtime, bench harness)" cargo test -q --release -p wasm-env -p bench-harness
step "unit tests (translator)" cargo test -q -p wasm-env-translate --features lift
# The GPU service: on the host's GPU, then on software Vulkan (lavapipe),
# where there is one; its kernels compile with $WASM_CC.
step "unit tests (GPU service)" cargo test -q --release -p wasm-env-gpu
step "unit tests (GPU service, software)" env WASM_ENV_GPU=software cargo test -q --release -p wasm-env-gpu device
step "M0: toolchain, runtime, BusyBox" tests/m0.sh
step "M1: processes and a shell" env SKIP_BUILD=1 tests/m1.sh
step "M2: arm64 translation (differential vs qemu)" tests/m2.sh
step "M2: translation on exec (runtime integration)" tests/m2-exec.sh
step "M3: dynamic linking (differential vs qemu)" tests/m3.sh
step "Alpine arm64 userland (differential vs qemu)" tests/alpine.sh
step "Python 3.14 from Alpine (differential vs qemu)" tests/python.sh
step "Ubuntu 24.04 arm64, glibc (differential vs qemu)" tests/ubuntu.sh
step "M5: converting ubuntu:24.04 and a shell in it" tests/convert.sh
exit $status
