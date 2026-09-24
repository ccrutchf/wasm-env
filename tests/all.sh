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
step "unit tests (translator)" cargo test -q -p wasm-env-translate
step "M0: toolchain, runtime, BusyBox" tests/m0.sh
step "M1: processes and a shell" env SKIP_BUILD=1 tests/m1.sh
step "M2: arm64 translation (differential vs qemu)" tests/m2.sh
step "M2: translation on exec (runtime integration)" tests/m2-exec.sh
exit $status
