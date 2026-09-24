#!/usr/bin/env bash
# M2 integration: the runtime translates arm64 ELF binaries on exec.
# Checks that `wasm-env run` on a raw arm64 ELF matches qemu, and that the
# (recompiled, wasm-native) BusyBox shell can exec a translated binary.
#
#   nix develop -c tests/m2-exec.sh
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
cargo build -q --release -p wasm-env -p wasm-env-translate --features lift || exit 1
export WASM_ENV_TRANSLATE=$root/target/release/wasm-env-translate
export WASM_ENV_CACHE=$root/build/m2-exec-cache
rm -rf "$WASM_ENV_CACHE"
wasm_env=$root/target/release/wasm-env

rootfs=$root/build/m2-exec-rootfs
rm -rf "$rootfs"; mkdir -p "$rootfs/bin"
aarch64-unknown-linux-musl-gcc -O2 -static -o "$rootfs/bin/hello" tests/c/hello.c
cp build/packages/busybox/busybox.wasm "$rootfs/bin/busybox" 2>/dev/null \
  || { packages/busybox/build.sh >/dev/null 2>&1 && cp build/packages/busybox/busybox.wasm "$rootfs/bin/busybox"; }
for a in $("$wasm_env" run --root "$rootfs" /bin/busybox --list); do ln -sf busybox "$rootfs/bin/$a"; done

passed=0; failed=0
check() { if [ "$2" = "$3" ]; then passed=$((passed+1)); else failed=$((failed+1)); printf 'FAIL %s\n  expected: %q\n  actual:   %q\n' "$1" "$2" "$3"; fi; }

# 1. Direct: `wasm-env run` on the raw ELF matches qemu (translated on first use).
env -i HOME=/root PATH=/usr/local/bin:/usr/bin:/bin TERM=xterm \
  "$(command -v qemu-aarch64)" "$rootfs/bin/hello" a "b c" >build/m2-exec.expected 2>&1
exp=$?
"$wasm_env" run --root "$rootfs" /bin/hello a "b c" >build/m2-exec.actual 2>&1
act=$?
sed -i 's|argv\[0\] = .*hello|argv[0] = PROG|' build/m2-exec.expected build/m2-exec.actual
check "direct run: exit status" "$exp" "$act"
check "direct run: output" "$(cat build/m2-exec.expected)" "$(cat build/m2-exec.actual)"

# 2. The translation is cached: a second run doesn't re-translate.
"$wasm_env" run --trace --root "$rootfs" /bin/hello 2>&1 | grep -q translating && retranslated=yes || retranslated=no
check "translation is cached" "no" "$retranslated"

# 3. Via the shell: hush execs the translated binary and sees its exit status.
out=$("$wasm_env" run --root "$rootfs" /bin/sh -c '/bin/hello x; echo exit=$?' 2>&1)
check "shell exec: runs translated binary" "hello from wasm64-linux-musl, argc=2" "$(sed -n 2p <<<"$out")"
check "shell exec: exit status" "exit=7" "$(printf '%s\n' "$out" | tail -1)"

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
