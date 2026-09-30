#!/usr/bin/env bash
# M0 acceptance test: builds the toolchain, runtime, a C test program and
# BusyBox, then checks their behavior under wasm-env.
#
#   nix develop -c tests/m0.sh
#
# The .wasm files are portable: to test another host, build them once and
# run this script there with SKIP_BUILD=1 and a wasm-env binary for that
# host in target/release/.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if [ "${SKIP_BUILD:-0}" != 1 ]; then
	toolchain/build.sh >/dev/null
	cargo build -q --release -p wasm-env
	toolchain/bin/wasm64-linux-musl-cc -O2 tests/c/hello.c -o build/hello.wasm
	packages/busybox/build.sh >/dev/null 2>&1
fi

# (WASM_ENV_RUNTIME: another build of the runtime, e.g. Windows' under Wine.)
wasm_env=${WASM_ENV_RUNTIME:-$root/target/release/wasm-env}
rootfs=$root/build/test-rootfs
rm -rf "$rootfs"
mkdir -p "$rootfs"/{etc,root,tmp,usr/share}
echo wasm-env > "$rootfs/etc/hostname"
echo 'root:x:0:0:root:/root:/bin/sh' > "$rootfs/etc/passwd"
echo 'root:x:0:' > "$rootfs/etc/group"
printf 'hello\nworld\nhello\nwasm\n' > "$rootfs/usr/share/words"
ln -s /etc/hostname "$rootfs/root/link-to-hostname"
head -c 1000000 /dev/urandom > "$rootfs/tmp/random.bin"

bb() { "$wasm_env" run --root "$rootfs" build/packages/busybox/busybox.wasm "$@"; }

passed=0
failed=0
check() {
	local name=$1 expected=$2 actual=$3
	if [ "$expected" = "$actual" ]; then
		passed=$((passed + 1))
	else
		failed=$((failed + 1))
		printf 'FAIL %s\n  expected: %q\n  actual:   %q\n' "$name" "$expected" "$actual"
	fi
}

# C program: startup, argv/env, malloc, long double, setjmp/longjmp, exit status.
out=$("$wasm_env" run --env HOME=/root build/hello.wasm a "b c" || echo "status $?")
check "hello: constructor" "constructor ran" "$(sed -n 1p <<<"$out")"
check "hello: argv" "  argv[2] = b c" "$(sed -n 5p <<<"$out")"
check "hello: env" "HOME=/root" "$(sed -n 6p <<<"$out")"
check "hello: long double" "long double: 0.333333, 128-bit: 1024" "$(sed -n 8p <<<"$out")"
check "hello: longjmp" "longjmp returned 42" "$(sed -n 9p <<<"$out")"
check "hello: exit status" "status 7" "$(sed -n 10p <<<"$out")"

# BusyBox.
check "echo" "hello from busybox" "$(bb echo hello from busybox)"
check "cat through absolute symlink" "wasm-env" "$(bb cat /root/link-to-hostname)"
check "ls" "etc root tmp usr" "$(bb ls / | tr '\n' ' ' | sed 's/ $//')"
check "ls -l owner" "root root" "$(bb ls -l /etc/hostname | awk '{print $3, $4}')"
check "readlink" "/etc/hostname" "$(bb ls -l /root/link-to-hostname | awk '{print $NF}')"
check "sort | uniq" "hello wasm world" "$(bb sort -u /usr/share/words | tr '\n' ' ' | sed 's/ $//')"
check "grep -c" "2" "$(bb grep -c hello /usr/share/words)"
check "uname" "Linux wasm64" "$(bb uname -sm)"
check "sha256 matches host" \
	"$(sha256sum "$rootfs/tmp/random.bin" 2>/dev/null | cut -c1-64 || shasum -a 256 "$rootfs/tmp/random.bin" | cut -c1-64)" \
	"$(bb sha256sum /tmp/random.bin | cut -c1-64)"
check "missing file exit status" "1" "$(bb ls /missing 2>/dev/null; echo $?)"
check "can't escape root" "etc root tmp usr" "$(bb ls /../../.. | tr '\n' ' ' | sed 's/ $//')"

# File operations.
bb mkdir -p /tmp/a/b
bb touch /tmp/a/b/f
bb cp -p /etc/hostname /tmp/a/copy
bb mv /tmp/a/copy /tmp/a/moved
check "mkdir/touch/cp/mv" "/tmp/a: b moved /tmp/a/b: f" "$(bb ls -R /tmp/a | tr '\n' ' ' | sed 's/  */ /g; s/ $//')"
check "file contents" "wasm-env" "$(cat "$rootfs/tmp/a/moved")"
bb rm -r /tmp/a
check "rm -r" "no" "$([ -e "$rootfs/tmp/a" ] && echo yes || echo no)"

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
