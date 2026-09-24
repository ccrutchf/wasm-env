#!/usr/bin/env bash
# Real distribution binaries: Alpine Linux's own arm64 userland (BusyBox,
# dynamically linked against Alpine's musl, with RELR relocations), run
# under wasm-env and compared with qemu-aarch64 running the same binaries.
# Fetches Alpine's minirootfs the first time (tests/alpine-fetch.sh); skips
# if that isn't possible (no network).
#
#   nix develop -c tests/alpine.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
if ! tests/alpine-fetch.sh >/dev/null 2>&1; then
	echo "skipped: can't fetch Alpine's minirootfs (offline?)"
	exit 0
fi
cargo build -q --release -p wasm-env -p wasm-env-translate --features wasm-env-translate/lift || exit 1
export WASM_ENV_CACHE=$root/build/alpine-wasm-cache
R=$root/build/alpine
wasm_env=$root/target/release/wasm-env

run_w() { timeout 60 "$wasm_env" run --root "$R" "$@" 2>&1; echo "[$?]"; }
# qemu runs busybox with the applet as argv[0]: Alpine's applet links are
# absolute symlinks, which qemu would resolve against the host.
run_q() { (cd "$R" && timeout 60 env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/root TERM=xterm \
	qemu-aarch64 -L . -0 "$1" ./bin/busybox "${@:2}" 2>&1; echo "[$?]"); }
passed=0; failed=0
t() {
	local w q
	w=$(run_w "$@" | od -c) q=$(run_q "$@" | od -c) # od: compare binary output exactly
	if [ "$w" = "$q" ]; then passed=$((passed+1)); else failed=$((failed+1)); echo "FAIL: $*"; fi
}

t /bin/cat /etc/os-release
t /bin/ls -1 /etc
t /usr/bin/wc -l /etc/services
t /bin/sed -n "s/^ID=//p" /etc/os-release
t /bin/sed -e "s/[aeiou]/_/g;3q" /etc/services
t /usr/bin/awk -F: '{ n[$7]++ } END { for (s in n) print s, n[s] }' /etc/passwd
t /usr/bin/awk 'BEGIN { for (i = 0; i < 5; i++) s += i*i; printf "%d %.3f %s\n", s, sqrt(2), substr("abcdef", 2, 3) }'
t /usr/bin/sort -t: -k3 -n /etc/passwd
t /bin/grep -E -o "[0-9]+/(tcp|udp)" /etc/services
t /usr/bin/cut -d: -f1,3 /etc/group
t /usr/bin/find /etc/apk -type f
t /bin/stat -c "%n %s %a" /etc/passwd
t /usr/bin/printf "%08.3e %o %c %-5s|\n" 12345.678 64 65 ab
t /bin/date -u -d @1234567890 "+%A %d %B %Y %T %j %s"
t /usr/bin/cal 2 2024
t /bin/busybox dc -e "2 64 ^ p"
t /usr/bin/bc -l -e "s(1)+c(1)"
t /usr/bin/factor 600851475143 4294967297
t /usr/bin/expr "abcdef" : ".*\(c.*\)"
t /bin/gzip -c /etc/services
t /usr/bin/md5sum /etc/services
t /usr/bin/sha512sum /etc/services
t /usr/bin/xxd -l 48 /etc/os-release
t /bin/busybox ash -c 'echo $((7*6)); x=abc; echo ${#x} ${x#a}'

# A native (wasm) shell running a pipeline of Alpine's translated programs.
mkdir -p "$R/usr/local/wasm"
cp build/packages/busybox/busybox.wasm "$R/usr/local/wasm/busybox"
ln -sf busybox "$R/usr/local/wasm/sh"
out=$(timeout 60 "$wasm_env" run --root "$R" /usr/local/wasm/sh -c \
	'/bin/cat /etc/passwd | /bin/grep -v nologin | /usr/bin/cut -d: -f1 | /usr/bin/sort -r | /usr/bin/tr "\n" " "' 2>&1)
if [ "$out" = "sync shutdown root halt " ]; then passed=$((passed+1)); else failed=$((failed+1)); echo "FAIL: native shell pipeline: $out"; fi

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
