#!/usr/bin/env bash
# Builds wasm-env's Vulkan driver (icd.c) for arm64 Linux environments: a
# shared library the Khronos Vulkan loader (libvulkan.so.1) loads, which the
# runtime translates like any other, and its manifest for the loader.
#
#   nix develop -c packages/vulkan/build.sh
#
# Output: build/packages/vulkan/libwasm-env-vulkan.so and
# wasm-env_icd.aarch64.json. It's freestanding (the program's own C
# library, glibc or musl, provides the few functions it uses), so clang and
# lld build it on any host.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
out=$root/build/packages/vulkan
: "${VULKAN_HEADERS:?run inside nix develop}" "${WASM_CC:?run inside nix develop}"
mkdir -p "$out"
"$WASM_CC" --target=aarch64-linux-gnu -ffreestanding -nostdlib -shared -fPIC -fuse-ld=lld -O2 \
	-Wall -Wextra -Wno-unused-parameter -Wno-cast-function-type-mismatch -Wno-missing-field-initializers \
	-fvisibility=hidden -fno-stack-protector -mno-outline-atomics -I"$VULKAN_HEADERS/include" \
	-Wl,-soname,libwasm-env-vulkan.so -o "$out/libwasm-env-vulkan.so" "$root/packages/vulkan/icd.c"
cat >"$out/wasm-env_icd.aarch64.json" <<'JSON'
{
    "file_format_version": "1.0.1",
    "ICD": {
        "library_path": "/usr/lib/wasm-env/libwasm-env-vulkan.so",
        "library_arch": "64",
        "api_version": "1.0.0"
    }
}
JSON
echo "$out/libwasm-env-vulkan.so"
