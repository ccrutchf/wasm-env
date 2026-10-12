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
# Its entry points, from the Vulkan registry: the functions icd.c defines,
# the rest stubs (entries.awk).
implemented=$(sed -n 's/.* CC \([A-Za-z0-9_]*\)(.*/\1/p' "$root/packages/vulkan/icd.c" | sort -u | tr '\n' ' ')
awk -v versions="1.0 1.1" -v extensions="VK_KHR_get_physical_device_properties2 VK_KHR_surface VK_KHR_wayland_surface VK_KHR_swapchain \
	VK_KHR_external_memory_capabilities VK_KHR_external_semaphore_capabilities VK_KHR_external_fence_capabilities VK_KHR_maintenance1 \
	VK_KHR_maintenance2 VK_KHR_maintenance3 VK_KHR_get_memory_requirements2 VK_KHR_dedicated_allocation VK_KHR_bind_memory2 \
	VK_KHR_descriptor_update_template VK_KHR_multiview VK_KHR_storage_buffer_storage_class VK_KHR_relaxed_block_layout" \
	-v commands="vkEnumerateInstanceVersion" -v omit="vkCreateWaylandSurfaceKHR vkDestroySurfaceKHR" \
	-v implemented="$implemented" -f "$root/packages/vulkan/entries.awk" \
	"$VULKAN_HEADERS/share/vulkan/registry/vk.xml" >"$out/entries.h"
"$WASM_CC" --target=aarch64-linux-gnu -ffreestanding -nostdlib -shared -fPIC -fuse-ld=lld -O2 \
	-Wall -Wextra -Wno-unused-parameter -Wno-cast-function-type-mismatch -Wno-missing-field-initializers \
	-fvisibility=hidden -fno-stack-protector -mno-outline-atomics -I"$VULKAN_HEADERS/include" -I"$out" \
	-Wl,-soname,libwasm-env-vulkan.so -o "$out/libwasm-env-vulkan.so" "$root/packages/vulkan/icd.c"
cat >"$out/wasm-env_icd.aarch64.json" <<'JSON'
{
    "file_format_version": "1.0.1",
    "ICD": {
        "library_path": "/usr/lib/wasm-env/libwasm-env-vulkan.so",
        "library_arch": "64",
        "api_version": "1.1.0"
    }
}
JSON
echo "$out/libwasm-env-vulkan.so"
