#!/usr/bin/env bash
# M5: converting an image and using it (PLAN.md, M5).
#
# Converts ubuntu:24.04 (arm64) with `wasm-env convert`: from Docker Hub when
# it's reachable, else from Ubuntu's base tarball (tests/ubuntu-fetch.sh).
# Then, in a shell in it (`wasm-env shell`), runs everyday commands (pipes,
# command substitution, fork, files, dpkg's database) against expected
# output, and, if Ubuntu's mirror is reachable, installs a package with apt
# and runs it (translated on first use).
#
#   nix develop -c tests/convert.sh      (FRESH=1 converts again)
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
cargo build -q --release -p wasm-env -p wasm-env-translate --features wasm-env-translate/lift || exit 1
wasm_env=$root/target/release/wasm-env
dir=$root/build/convert/ubuntu

if [ "${FRESH:-0}" = 1 ] || [ ! -e "$dir/.wasm-env-image" ]; then
	rm -rf "$dir"
	if curl -sS -o /dev/null --max-time 10 https://registry-1.docker.io/v2/ 2>/dev/null; then
		source=ubuntu:24.04
	else
		tests/ubuntu-fetch.sh >/dev/null || exit 1
		source=$root/build/ubuntu-cache/ubuntu-base-24.04.5-base-arm64.tar.gz
	fi
	"$wasm_env" convert "$source" "$dir" 2>&1 | grep -v '^  \[' || exit 1
fi

passed=0; failed=0
check() { # name, expected, command
	local actual
	actual=$(timeout 600 "$wasm_env" shell "$dir" -c "$3" 2>&1)
	if [ "$2" = "$actual" ]; then passed=$((passed+1)); else failed=$((failed+1)); echo "FAIL $1"; diff <(echo "$2") <(echo "$actual") | head -8; fi
}

check "os-release" 'PRETTY_NAME="Ubuntu 24.04.5 LTS"' "grep PRETTY /etc/os-release"
check "pipes and substitution" "3 aarch64" 'echo $(printf "a\nb\nc\n" | wc -l) $(uname -m)'
check "fork in a loop" "10" 'n=0; for i in $(seq 10); do n=$((n + $(echo 1))); done; echo $n'
check "dpkg database" "Status: install ok installed" "dpkg -s coreutils | grep Status"
check "files" "hello
ls: cannot access 'x': No such file or directory
gone" 'cd /tmp && echo hello > x && cat x && rm x && ls x; mkdir -p d/e && rmdir d/e d && echo gone'
check "ls -l" "-rw-r--r--" 'ls -l /etc/passwd | cut -c1-10'
check "df" "Mounted on" 'df -h / | head -1 | grep -o "Mounted on"'
check "grep, awk, sed, sort" "bin
daemon
root" "grep -E '^(root|daemon|bin):' /etc/passwd | awk -F: '{print \$1}' | sed 's/x//' | sort -r | sort"
check "tar and gzip" "etc/hostname" 'tar czf /tmp/e.tgz -C / etc/hostname && tar tzf /tmp/e.tgz && rm /tmp/e.tgz'
check "perl" "42" 'perl -e "print 6 * 7, qq(\n)"'
check "users and groups" "uid=0(root) gid=0(root) groups=0(root)" 'id'

if curl -sS -o /dev/null --max-time 10 http://ports.ubuntu.com/ubuntu-ports/ 2>/dev/null; then
	check "apt-get install" "Hello, world!
Status: install ok installed" 'DEBIAN_FRONTEND=noninteractive apt-get update >/dev/null 2>&1; DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends hello >/dev/null 2>&1; hello; dpkg -s hello | grep Status'
else
	echo "skipped apt: the Ubuntu mirror isn't reachable"
fi

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
