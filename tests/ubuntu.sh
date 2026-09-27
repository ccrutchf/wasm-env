#!/usr/bin/env bash
# Ubuntu 24.04 arm64 (glibc): the C fixtures built against Ubuntu's glibc
# (clang with the Ubuntu root as its sysroot), Ubuntu's own coreutils, and
# Ubuntu's python3.12, each run under wasm-env and compared with
# qemu-aarch64. Fetches packages the first time (tests/ubuntu-fetch.sh) and
# skips if that isn't possible.
#
#   nix develop -c tests/ubuntu.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
if ! tests/ubuntu-fetch.sh libc6-dev libgcc-13-dev python3.12-minimal >/dev/null 2>&1; then
	echo "skipped: can't fetch Ubuntu packages (offline?)"
	exit 0
fi
cargo build -q --release -p wasm-env -p wasm-env-translate --features wasm-env-translate/lift || exit 1
export WASM_ENV_CACHE=$root/build/ubuntu-wasm-cache
R=$root/build/ubuntu
wasm_env=$root/target/release/wasm-env
gcc_lib=$R/usr/lib/gcc/aarch64-linux-gnu/13
cc=("$WASM_CC" --target=aarch64-linux-gnu --sysroot="$R" -fuse-ld=lld -B"$gcc_lib" -L"$gcc_lib" -lm
	-Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1)
mkdir -p "$R/opt/tests" "$R/etc/wasm-env"

passed=0; failed=0
compare() { # name, expected, actual
	if [ "$2" = "$3" ]; then passed=$((passed+1)); else failed=$((failed+1)); echo "FAIL $1"; diff <(echo "$2") <(echo "$3") | head -8; fi
}
q() { (cd "$R" && timeout 120 env -i PATH=/usr/bin:/bin HOME=/root TERM=xterm qemu-aarch64 -L . -0 "$1" ".$1" "${@:2}" 2>&1 | od -c; echo "[${PIPESTATUS[0]}]"); }
w() { timeout 900 "$wasm_env" run --root "$R" "$@" 2>&1 | od -c; echo "[${PIPESTATUS[0]}]"; }
t() { compare "$*" "$(q "$@")" "$(w "$@")"; }

# The C fixtures, against glibc.
for f in c/hello.c arm64/control.c arm64/memory.c arm64/arith.c arm64/fp.c arm64/computedgoto.c arm64/simd3.c arm64/calls.c; do
	name=$(basename "$f" .c)
	flags=(-O2)
	[ "$name" = simd3 ] && flags+=(-march=armv8.2-a+crc+sha3)
	"${cc[@]}" "${flags[@]}" -o "$R/opt/tests/$name" "tests/$f" || { compare "$name: build" ok failed; continue; }
	t "/opt/tests/$name" a "b c"
done
# Creating processes (vfork, posix_spawn, system) from translated code.
# Against a written expectation: qemu-user can't exec arm64 programs from
# inside the guest.
"${cc[@]}" -O2 -o "$R/opt/tests/spawn" tests/arm64/spawn.c &&
	compare spawn "$(cat tests/arm64/spawn.expected)" "$(timeout 600 "$wasm_env" run --root "$R" /opt/tests/spawn 2>&1)"
# Threads through glibc's pthreads: exclusives, then LSE atomics (clang's
# default outline atomics, which pick LSE by AT_HWCAP).
"${cc[@]}" -O2 -pthread -mno-outline-atomics -o "$R/opt/tests/threads" tests/arm64/threads.c && t /opt/tests/threads
"${cc[@]}" -O2 -pthread -o "$R/opt/tests/threads-lse" tests/arm64/threads.c && t /opt/tests/threads-lse
# Signal handlers (frames, masks, sigsuspend, siglongjmp, EINTR, SIGCHLD).
"${cc[@]}" -O2 -o "$R/opt/tests/signals" tests/arm64/signals.c && t /opt/tests/signals
# Atomics (LSE) and dlopen through glibc's dynamic linker.
"${cc[@]}" -O2 -march=armv8.1-a -o "$R/opt/tests/atomics" tests/arm64/atomics.c && t /opt/tests/atomics
"${cc[@]}" -O2 -shared -fPIC -o "$R/usr/lib/libgreet.so" tests/arm64/libgreet.c
"${cc[@]}" -O2 -shared -fPIC -o "$R/usr/lib/libplugin.so" tests/arm64/plugin.c -L"$R/usr/lib" -lgreet
"${cc[@]}" -O2 -o "$R/opt/tests/greet" tests/arm64/greet.c -L"$R/usr/lib" -lgreet -ldl
echo "greet /usr/lib/libplugin.so" > "$R/etc/wasm-env/dlopen"
t /opt/tests/greet you

# Ubuntu's own programs (paths inside the root only: qemu reads the host's
# files for paths the root lacks).
t /usr/bin/cat /etc/os-release
t /usr/bin/sort -t: -k3 -n /etc/passwd
t /usr/bin/sha256sum /etc/os-release
t /usr/bin/uname -m
t /usr/bin/printf "%08.3f|%x|%-5s|%e\n" 3.14159 255 ab 12345.678
t /usr/bin/mawk 'BEGIN { for (i = 0; i < 20000; i++) a[i] = i "x"; n = 0; for (k in a) n += length(a[k]); print n }'
t /usr/bin/find /usr/share/doc -name copyright
t /usr/bin/python3.12 -c 'import sys; print(1 + 1, sys.version_info[:2])'

# numpy and pandas from PyPI's manylinux wheels (numpy bundles OpenBLAS),
# stdout and stderr (numpy's floating-point RuntimeWarnings come from FPSR
# exception flags). qemu runs without SVE so OpenBLAS picks the same NEON
# kernels.
wheels=(numpy==2.5.3 pandas==3.0.6 python-dateutil==2.9.0.post0 six==1.17.0 tzdata==2026.4)
if tests/wheel-fetch.sh "${wheels[@]}" >/dev/null 2>&1; then
	site=/usr/local/lib/python3.12/dist-packages
	echo "python3* /usr/lib/python3.12/lib-dynload $site/numpy $site/pandas" >> "$R/etc/wasm-env/dlopen"
	# The first run translates Python with numpy and pandas (with a cold
	# cache, tens of minutes), hence the long timeout.
	for demo in numpy pandas; do
		cp "tests/python/${demo}_demo.py" "$R/root/"
		expected=$(cd "$R" && env -i HOME=/root OPENBLAS_NUM_THREADS=1 QEMU_CPU=max,sve=off,sme=off \
			qemu-aarch64 -L . -0 /usr/bin/python3.12 ./usr/bin/python3.12 "/root/${demo}_demo.py" 2>&1)
		actual=$(OPENBLAS_NUM_THREADS=1 timeout 7200 "$wasm_env" run --root "$R" /usr/bin/python3.12 "/root/${demo}_demo.py" 2>&1)
		compare "$demo demo" "$expected" "$actual"
	done
else
	echo "skipped numpy and pandas: can't fetch the wheels (offline?)"
fi

# Python threads (threading, queues, pools; hashlib in parallel), with
# Python's extension modules (numpy's rule above includes them).
grep -q '^python3' "$R/etc/wasm-env/dlopen" || echo "python3* /usr/lib/python3.12/lib-dynload" >> "$R/etc/wasm-env/dlopen"
cp tests/python/threads_demo.py "$R/root/"
t /usr/bin/python3.12 /root/threads_demo.py
# Python signal handlers, KeyboardInterrupt, a handler ending a blocking read, setitimer.
cp tests/python/signals_demo.py "$R/root/"
t /usr/bin/python3.12 -u /root/signals_demo.py

# PyTorch's CPU wheel (from PyTorch's index, not PyPI) and its pure-Python
# dependencies: a small model's forward pass, training steps and linear
# algebra. oneDNN is disabled in the demo: it generates code at run time,
# which ahead-of-time translation can't run. The first translation of
# libtorch takes about two hours and 11 GB (wasmtime's compile).
torch=torch-2.9.1%2Bcpu-cp312-cp312-manylinux_2_28_aarch64.whl
torch_sha=3bf9b442a51a2948e41216a76d7ab00f0694cfcaaa51b6f9bcab57b7f89843e6
site=/usr/local/lib/python3.12/dist-packages
mkdir -p build/torch
if { [ -e build/torch/torch-cpu.whl ] || curl -sSfL -o build/torch/torch-cpu.whl "https://download.pytorch.org/whl/cpu/$torch"; } \
	&& echo "$torch_sha  build/torch/torch-cpu.whl" | sha256sum -c --quiet \
	&& tests/wheel-fetch.sh filelock==3.20.0 typing_extensions==4.15.0 setuptools==80.9.0 sympy==1.14.0 \
		mpmath==1.3.0 networkx==3.5 jinja2==3.1.6 markupsafe==3.0.3 fsspec==2025.10.0 >/dev/null 2>&1; then
	[ -e "$R$site/.installed-torch-2.9.1" ] || { unzip -qo build/torch/torch-cpu.whl -d "$R$site" && touch "$R$site/.installed-torch-2.9.1"; }
	cp tests/python/torch_demo.py "$R/root/"
	expected=$(cd "$R" && env -i HOME=/root QEMU_CPU=max,sve=off,sme=off \
		qemu-aarch64 -L . -0 /usr/bin/python3.12 ./usr/bin/python3.12 /root/torch_demo.py 2>&1)
	actual=$(timeout 10800 "$wasm_env" run --root "$R" /usr/bin/python3.12 /root/torch_demo.py 2>&1)
	compare "torch demo" "$expected" "$actual"
else
	echo "skipped PyTorch: can't fetch the wheels (offline?)"
fi

# Networking, if PyPI is reachable: DNS (glibc's resolver, over the host's
# resolv.conf), HTTPS (Ubuntu's OpenSSL with its CA certificates), and pip
# installing a package from PyPI. Ubuntu's ca-certificates package builds
# its bundle when installed; here the fetch script only unpacks, so do that.
if curl -sSf -o /dev/null https://pypi.org/simple/ 2>/dev/null \
	&& tests/ubuntu-fetch.sh ca-certificates >/dev/null 2>&1 && tests/wheel-fetch.sh pip==26.2.1 >/dev/null 2>&1; then
	mkdir -p "$R/etc/ssl/certs"
	cat "$R"/usr/share/ca-certificates/mozilla/*.crt > "$R/etc/ssl/certs/ca-certificates.crt"
	# pip draws its progress bars from a thread.
	rm -f "$R/etc/pip.conf"
	py() { timeout 1800 "$wasm_env" run --root "$R" --env HOME=/root --env PATH=/usr/local/bin:/usr/bin:/bin /usr/bin/python3.12 "$@" 2>&1; }
	compare "DNS and HTTPS" "resolved: True
200 six 1.17.0" "$(py -c '
import json, socket, urllib.request
print("resolved:", bool(socket.getaddrinfo("pypi.org", 443, type=socket.SOCK_STREAM)))
r = urllib.request.urlopen("https://pypi.org/pypi/six/1.17.0/json", timeout=60)
print(r.status, json.load(r)["info"]["name"], "1.17.0")')"
	rm -rf "$R/tmp/pip-test"
	py -m pip install --disable-pip-version-check --root-user-action=ignore --target /tmp/pip-test attrs==25.3.0 >/dev/null
	compare "pip install from PyPI" "attrs 25.3.0" "$(py -c 'import sys; sys.path.insert(0, "/tmp/pip-test"); import attrs; print("attrs", attrs.__version__)')"
else
	echo "skipped networking: PyPI isn't reachable"
fi

echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
