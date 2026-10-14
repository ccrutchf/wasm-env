#!/usr/bin/env bash
# Prepares what another host needs to check wasm-env there (FOR-WINDOWS.md,
# FOR-MACOS.md): the Ubuntu environment (build/ubuntu) as an OCI archive,
# with the GPU drivers built from this tree, in export/.
#
#   nix develop -c tests/export.sh [--windows] [OUT.tar]
#
# OUT defaults to export/ubuntu-oci.tar, with its SHA-256 beside it. The
# archive leaves out the environment's translation cache (each host
# translates for itself), so the other host converts it:
# `wasm-env convert --lazy --no-precompile OUT.tar DIR`.
#
# --windows also builds the Windows executables (tests/cross.sh --build) and
# lays them out with the translator's Windows toolchain in export/windows/,
# as FOR-WINDOWS.md expects: bin\wasm-env.exe, bin\wasm-env-translate.exe,
# lib\wasm-env\toolchain\.
#
# Needs build/ubuntu (tests/ubuntu.sh makes it). The conformance tests and
# vkQuake in it are optional: their absence is reported, not an error.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
: "${WASM_CC:?run inside nix develop}"
R=$root/build/ubuntu
windows=
out=
for a in "$@"; do
	case "$a" in
	--windows) windows=1 ;;
	-*) sed -n '2,19p' "$0"; exit 1 ;;
	*) out=$a ;;
	esac
done
out=${out:-$root/export/ubuntu-oci.tar}
[ -d "$R/usr" ] || { echo "no environment in build/ubuntu: run tests/ubuntu.sh first" >&2; exit 1; }

echo "== the runtime"
cargo build --release -q

# The drivers from this tree, as tests/ubuntu.sh installs them: an export
# never carries older ones than the source.
echo "== the GPU drivers"
packages/vulkan/build.sh >/dev/null
packages/opencl/build.sh >/dev/null
mkdir -p "$R/usr/lib/wasm-env" "$R/usr/share/vulkan/icd.d" "$R/etc/OpenCL/vendors"
cp build/packages/vulkan/libwasm-env-vulkan.so build/packages/opencl/libwasm-env-opencl.so "$R/usr/lib/wasm-env/"
cp build/packages/vulkan/wasm-env_icd.aarch64.json "$R/usr/share/vulkan/icd.d/"
echo /usr/lib/wasm-env/libwasm-env-opencl.so >"$R/etc/OpenCL/vendors/wasm-env.icd"

# What the guides run, and where each comes from if it's missing.
echo "== what's in it"
check() { # path in the environment, what it is, how to get it
	if [ -e "$R$1" ]; then echo "  $2: $1"; else echo "  $2: missing ($3)"; fi
}
check /opt/tests/vulkan "the Vulkan driver's test" "tests/ubuntu.sh"
check /usr/bin/vkcube-wayland "vkcube" "tests/ubuntu.sh"
check /opt/vkquake/vkquake "vkQuake" "packages/vkquake/build.sh"
check /opt/cts/test_basic "OpenCL's conformance tests" "built by hand: build/opencl-cts"
check /opt/vk-cts/deqp-vk "Vulkan's conformance tests" "tests/vulkan-cts.sh build"

echo "== exporting to $out"
mkdir -p "$(dirname "$out")"
rm -f "$out" "$out.sha256"
target/release/wasm-env export "$R" "$out"
(cd "$(dirname "$out")" && sha256sum "$(basename "$out")" >"$(basename "$out").sha256")
ls -lh "$out" | awk '{print "  " $5 "  " $NF}'

if [ -n "$windows" ]; then
	echo "== the Windows executables"
	tests/cross.sh --build
	[ -f build/win-toolchain/bin/clang.exe ] || { echo "no Windows toolchain in build/win-toolchain (docs/hosts.md, \"The toolchain on Windows\")" >&2; exit 1; }
	w=$root/export/windows
	rm -rf "$w"
	mkdir -p "$w/bin" "$w/lib/wasm-env"
	cp build/target-cross/x86_64-pc-windows-gnu/release/wasm-env.exe build/target-cross/x86_64-pc-windows-gnu/release/wasm-env-translate.exe "$w/bin/"
	cp -rL build/win-toolchain "$w/lib/wasm-env/toolchain"
	echo "  $w (copy it to C:\\wasm-env, with $(basename "$out"))"
fi
