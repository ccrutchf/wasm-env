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

# 4. A dynamically-linked program: the runtime finds its dynamic linker and
# libraries in the environment, plus the plugin it dlopens, which
# /etc/wasm-env/dlopen declares.
: "${AARCH64_MUSL:?run inside nix develop}"
mkdir -p "$rootfs/lib" "$rootfs/usr/lib" "$rootfs/etc/wasm-env"
cp "$AARCH64_MUSL/lib/libc.so" "$rootfs/lib/ld-musl-aarch64.so.1"
cc=aarch64-unknown-linux-musl-gcc
$cc -O2 -shared -fPIC -o "$rootfs/usr/lib/libgreet.so" tests/arm64/libgreet.c
$cc -O2 -shared -fPIC -o "$rootfs/usr/lib/libplugin.so" tests/arm64/plugin.c -L"$rootfs/usr/lib" -lgreet
$cc -O2 -o "$rootfs/bin/greet" tests/arm64/greet.c -L"$rootfs/usr/lib" -lgreet -Wl,--dynamic-linker=/lib/ld-musl-aarch64.so.1
echo "greet* /usr/lib/libplugin.so" > "$rootfs/etc/wasm-env/dlopen"
expected=$(env -i HOME=/root PATH=/bin qemu-aarch64 -L "$rootfs" "$rootfs/bin/greet" shell)
out=$("$wasm_env" run --root "$rootfs" /bin/sh -c '/bin/greet shell; echo status=$?' 2>&1)
check "dynamic program via the shell" "$expected
status=0" "$out"

# 5. The cache notices a changed library: rebuild libgreet with a different
# greeting, and the next exec retranslates.
sed 's/hello, /HELLO, /' tests/arm64/libgreet.c > "$root/build/libgreet-v2.c"
$cc -O2 -shared -fPIC -o "$rootfs/usr/lib/libgreet.so" "$root/build/libgreet-v2.c"
out=$("$wasm_env" run --root "$rootfs" /bin/greet again 2>&1 | sed -n 2p)
check "cache retranslates after a library changes" "HELLO, world (call 1)" "$out"

# 6. A jump target computed at run time, which the translation missed: the
# run translates its page again and goes on, and records the target, so
# the next exec's translation has it (and needs no patch).
$cc -static -nostdlib -o "$rootfs/bin/hidden" tests/arm64/hidden.S
"$wasm_env" run --trace --root "$rootfs" /bin/hidden a b >"$root/build/hidden.1" 2>&1
check "missed target: the run goes on" "42" "$?"
check "missed target: its page translated again" "1" "$(grep -c "translated page" "$root/build/hidden.1")"
check "missed target: recorded" "1" "$(cat "$WASM_ENV_CACHE"/translated/*.roots 2>/dev/null | grep -c hidden)"
"$wasm_env" run --trace --root "$rootfs" /bin/hidden a b >"$root/build/hidden.2" 2>&1
check "missed target: next run translates it" "42 0" "$? $(grep -c "translated page" "$root/build/hidden.2")"

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
