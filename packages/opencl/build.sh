#!/usr/bin/env bash
# Builds wasm-env's OpenCL driver (icd.c) for arm64 Linux environments: a
# shared library the Khronos ICD loader (ocl-icd) loads, which the runtime
# translates like any other.
#
#   nix develop -c packages/opencl/build.sh
#
# Output: build/packages/opencl/libwasm-env-opencl.so. It's freestanding
# (the program's own C library, glibc or musl, provides the few functions
# it uses), so clang and lld build it on any host.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
out=$root/build/packages/opencl
: "${OPENCL_HEADERS:?run inside nix develop}" "${WASM_CC:?run inside nix develop}"
mkdir -p "$out"
"$WASM_CC" --target=aarch64-linux-gnu -ffreestanding -nostdlib -shared -fPIC -fuse-ld=lld -O2 \
	-Wall -Wextra -Wno-unused-parameter -Wno-cast-function-type-mismatch -Wno-incompatible-function-pointer-types -Wno-missing-field-initializers \
	-fvisibility=hidden -fno-stack-protector -mno-outline-atomics -I"$OPENCL_HEADERS/include" \
	-Wl,-soname,libwasm-env-opencl.so -o "$out/libwasm-env-opencl.so" "$root/packages/opencl/icd.c"
echo "$out/libwasm-env-opencl.so"
