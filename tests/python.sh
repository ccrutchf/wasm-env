#!/usr/bin/env bash
# Alpine Linux's Python 3.14 (arm64, musl) with its extension modules,
# translated on exec and compared with qemu-aarch64. Fetches the packages
# the first time (tests/alpine-fetch.sh) and skips if that isn't possible.
# The first run translates ~660k blocks (a few minutes); later runs use the
# translation, object and module caches.
#
#   nix develop -c tests/python.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
if ! tests/alpine-fetch.sh python3 >/dev/null 2>&1; then
	echo "skipped: can't fetch Alpine's python3 (offline?)"
	exit 0
fi
cargo build -q --release -p wasm-env -p wasm-env-translate --features wasm-env-translate/lift || exit 1
export WASM_ENV_CACHE=$root/build/alpine-wasm-cache
R=$root/build/alpine
wasm_env=$root/target/release/wasm-env
mkdir -p "$R/etc/wasm-env" "$R/root"
# Python loads its extension modules with dlopen: declare them.
echo "python3* /usr/lib/python3.14/lib-dynload" > "$R/etc/wasm-env/dlopen"
cp tests/python/demo.py "$R/root/demo.py"

passed=0; failed=0
check() { if [ "$2" = "$3" ]; then passed=$((passed+1)); else failed=$((failed+1)); printf 'FAIL %s\n' "$1"; diff <(echo "$2") <(echo "$3") | head -10; fi; }
q() { (cd "$R" && timeout 300 env -i PATH=/usr/bin:/bin HOME=/root qemu-aarch64 -L . ./usr/bin/python3.14 "$@" 2>&1; echo "[$?]"); }
w() { timeout 900 "$wasm_env" run --root "$R" /usr/bin/python3.14 "$@" 2>&1; echo "[$?]"; }

check "print" "$(q -c 'print(1 + 1, "from python")')" "$(w -c 'print(1 + 1, "from python")')"
check "extension modules" "$(q /root/demo.py)" "$(w /root/demo.py)"
check "exceptions and exit status" "$(q -c 'import sys; print(sys.version_info[:2]); raise SystemExit(3)')" \
	"$(w -c 'import sys; print(sys.version_info[:2]); raise SystemExit(3)')"

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
