#!/usr/bin/env bash
# Khronos's Vulkan conformance tests (VK-GL-CTS's deqp-vk) against wasm-env's
# Vulkan driver: built for arm64 against the Ubuntu root (build/ubuntu,
# with the driver installed by tests/ubuntu.sh), headless (dEQP's vulkan_headless
# target: the environment's libvulkan, no window system), installed in its /opt/vk-cts, and run there.
#
#   nix develop -c tests/vulkan-cts.sh build
#   nix develop -c tests/vulkan-cts.sh run [GPU] PATTERN...
#
# `run` runs the cases matching each pattern (dEQP's: `dEQP-VK.api.*`) on
# lavapipe (GPU software, the default) or the Intel GPU (intel), a pattern
# at a time, and counts the results in build/vk-cts-run/PATTERN.log
# (deqp-vk's own log is PATTERN.qpa). Needs network the first time (the
# CTS's sources and theirs). WASM_ENV names another wasm-env to run it with,
# ICD_JSON another driver manifest in the environment's icd.d.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
R=$root/build/ubuntu
src=$root/build/vk-cts
out=$root/build/vk-cts-build
runs=$root/build/vk-cts-run
version=vulkan-cts-1.3.10.0
: "${WASM_CC:?run inside nix develop}"

case "${1:-}" in
build)
	if [ ! -d "$src" ]; then
		git clone -q --depth 1 --branch "$version" https://github.com/KhronosGroup/VK-GL-CTS.git "$src"
		(cd "$src" && nix shell nixpkgs#python3 nixpkgs#git -c python3 external/fetch_sources.py >/dev/null)
	fi
	gcc_lib=$R/usr/lib/gcc/aarch64-linux-gnu/13
	bin=$(dirname "$WASM_CC")
	mkdir -p "$out"
	cat >"$out/toolchain.cmake" <<EOF
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR aarch64)
set(CMAKE_SYSROOT $R)
set(CMAKE_C_COMPILER $bin/clang)
set(CMAKE_CXX_COMPILER $bin/clang++)
set(CMAKE_C_COMPILER_TARGET aarch64-linux-gnu)
set(CMAKE_CXX_COMPILER_TARGET aarch64-linux-gnu)
set(CMAKE_C_FLAGS_INIT "-B$gcc_lib")
set(CMAKE_CXX_FLAGS_INIT "-B$gcc_lib -Wno-missing-template-arg-list-after-template-kw")
set(CMAKE_EXE_LINKER_FLAGS_INIT "-fuse-ld=lld -B$gcc_lib -L$gcc_lib -L$R/usr/lib/aarch64-linux-gnu -Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
EOF
	nix shell nixpkgs#cmake nixpkgs#ninja nixpkgs#python3 -c bash -c "
		set -e
		cmake -S '$src' -B '$out' -G Ninja -DCMAKE_TOOLCHAIN_FILE='$out/toolchain.cmake' \
			-DCMAKE_BUILD_TYPE=Release -DDEQP_TARGET=vulkan_headless -DSELECTED_BUILD_TARGETS=deqp-vk >'$out/configure.log' 2>&1 ||
			{ tail -30 '$out/configure.log'; exit 1; }
		ninja -C '$out' -j6 deqp-vk >'$out/build.log' 2>&1 || { tail -30 '$out/build.log'; exit 1; }
	"
	# The binary and the data it reads (its shaders' sources, images), in
	# the environment.
	mkdir -p "$R/opt/vk-cts"
	cp "$out/external/vulkancts/modules/vulkan/deqp-vk" "$R/opt/vk-cts/"
	rm -rf "$R/opt/vk-cts/vulkan"
	cp -r "$out/external/vulkancts/modules/vulkan/vulkan" "$R/opt/vk-cts/" 2>/dev/null || true
	cp -r "$src/external/vulkancts/data/vulkan" "$R/opt/vk-cts/" 2>/dev/null || true
	echo "$R/opt/vk-cts"
	;;
run)
	shift
	icd=lvp gpu=software
	if [ "${1:-}" = intel ] || [ "${1:-}" = software ]; then
		[ "$1" = intel ] && icd=intel gpu=intel
		shift
	fi
	# (A directory of the run's own, in the environment's /tmp: runs side
	# by side don't share their lists of cases.)
	tmp=vk-cts.$$
	trap 'rm -rf "$R/tmp/$tmp"' EXIT
	mkdir -p "$runs" "$R/tmp/$tmp"
	cts() { # deqp-vk ARGS... in the environment, from DIR (its data /opt/vk-cts's)
		local dir=$1
		shift
		VK_ICD_FILENAMES=$root/build/mesa/share/vulkan/icd.d/${icd}_icd.x86_64.json WGPU_BACKEND=vulkan WASM_ENV_GPU=$gpu \
			timeout "${CTS_TIMEOUT:-7200}" "${WASM_ENV:-$root/target/release/wasm-env}" run --root "$R" \
			--env VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/${ICD_JSON:-wasm-env_icd.aarch64.json} --env HOME=/root \
			/bin/sh -c "cd $dir && /opt/vk-cts/deqp-vk --deqp-archive-dir=/opt/vk-cts $*"
	}
	for pattern in "$@"; do
		name=${pattern//\*/all}
		log=$runs/$name.log
		: >"$log"
		# Its cases, then runs of those not yet run: a crash ends a run, so
		# the case it was in counts as crashed and the next run goes on
		# after it.
		cts "/tmp/$tmp" "--deqp-runmode=txt-caselist --deqp-case='$pattern' --deqp-log-filename=/dev/null" >/dev/null 2>&1 || true
		grep '^TEST: ' "$R/tmp/$tmp/dEQP-VK-cases.txt" | cut -d' ' -f2 >"$R/tmp/$tmp/left.txt" 2>/dev/null || true
		rm -f "$R/tmp/$tmp/dEQP-VK-cases.txt" "$runs/$name.qpa"
		while [ -s "$R/tmp/$tmp/left.txt" ]; do
			cts /opt/vk-cts "--deqp-caselist-file=/tmp/$tmp/left.txt --deqp-log-filename=/tmp/$tmp/part.qpa --deqp-log-images=disable --deqp-log-shader-sources=disable" >>"$log" 2>&1 || true
			qpa=$R/tmp/$tmp/part.qpa
			cat "$qpa" >>"$runs/$name.qpa" 2>/dev/null || true
			# (Cases begun: done, or the crash's.)
			grep -a -o '^#beginTestCaseResult .*' "$qpa" 2>/dev/null | cut -d' ' -f2 >"$R/tmp/$tmp/begun.txt" || true
			last=$(tail -1 "$R/tmp/$tmp/begun.txt")
			if [ -n "$last" ] && ! tail -5 "$qpa" | grep -a -q '^#endTestCaseResult\|^#endSession'; then
				echo "#crash $last" >>"$runs/$name.qpa"
			fi
			[ -s "$R/tmp/$tmp/begun.txt" ] || break
			grep -v -x -F -f "$R/tmp/$tmp/begun.txt" "$R/tmp/$tmp/left.txt" >"$R/tmp/$tmp/next.txt" || true
			mv "$R/tmp/$tmp/next.txt" "$R/tmp/$tmp/left.txt"
		done
		# The count: each case's status, and the crashes.
		counts=$(grep -a -o 'StatusCode="[A-Za-z]*"' "$runs/$name.qpa" 2>/dev/null | cut -d'"' -f2 | sort | uniq -c | awk '{printf "%s %s, ", $2, $1}')
		crashes=$(grep -a -c '^#crash' "$runs/$name.qpa" 2>/dev/null || true)
		echo "$pattern ($gpu): ${counts}Crash ${crashes:-0}"
	done
	;;
*)
	sed -n '2,16p' "$0"
	exit 1
	;;
esac
