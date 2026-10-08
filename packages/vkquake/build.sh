#!/usr/bin/env bash
# Builds vkQuake 1.36.0 (Quake on Vulkan; its swapchain RGBA8, which the
# driver swaps to BGRA as it presents) for arm64 against the
# Ubuntu root (build/ubuntu, with libsdl2-dev) and installs it in its
# /opt/vkquake, with Quake's shareware data (id1/pak0.pak, freely
# redistributable): a test of wasm-env's Vulkan driver (tests/ubuntu.sh)
# with a real game's renderer. Built with the dev shell's clang, Vulkan
# headers and glslang (its shaders, as SPIR-V); no MP3 or Vorbis music (the
# shareware has none).
#
#   nix develop -c packages/vkquake/build.sh
#
# Output: build/ubuntu/opt/vkquake. Needs network the first time (the
# release tarball and the shareware archive, checked against their SHA-256
# sums).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
R=$root/build/ubuntu
src=$root/build/vkquake-src
: "${WASM_CC:?run inside nix develop}" "${VULKAN_HEADERS:?run inside nix develop}"
mkdir -p "$src"
cd "$src"
version=1.36.0
fetch() { # name, url, sha256
	[ -f "$1" ] || { curl -sSfL -o "$1.part" "$2" && mv "$1.part" "$1"; }
	echo "$3  $1" | sha256sum -c --quiet
}
fetch vkQuake-$version.tar.gz https://github.com/Novum/vkQuake/archive/refs/tags/$version.tar.gz \
	3dd1bd8c13e902db9c3d216ce065552cb975dfd8b1b17a21b49c5458eb8a306e
fetch quake106.zip https://archive.org/download/quakeshareware/QUAKE_SW.zip \
	b8e3e9c9f875dc6dda5ebdb9c2434bdfb3ece86c516089ebfe5c12106fffe7c1
[ -d vkQuake-$version ] || tar -xzf vkQuake-$version.tar.gz
gcc_lib=$R/usr/lib/gcc/aarch64-linux-gnu/13
cc="$WASM_CC --target=aarch64-linux-gnu --sysroot=$R -B$gcc_lib"
ldflags="-fuse-ld=lld -L$gcc_lib -L$R/usr/lib/aarch64-linux-gnu -Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1"
# (CFLAGS by the environment: its Makefile adds to them. SDL2's, Ubuntu's,
# by hand: no sdl2-config here. -Wno-error after its own -Werror.)
(cd vkQuake-$version/Quake && CFLAGS="-O2 -D_GNU_SOURCE -I$VULKAN_HEADERS/include" LDFLAGS="$ldflags" make -j8 \
	CC="$cc" HOST_CC=cc SDL_CONFIG=true USE_SDL3=0 SDL_CFLAGS="-I$R/usr/include/SDL2 -D_REENTRANT -Wno-error" SDL_LIBS="-lSDL2" \
	USE_CODEC_MP3=0 USE_CODEC_VORBIS=0 vkquake >"$src/make.log" 2>&1) || { tail -30 "$src/make.log"; exit 1; }
mkdir -p "$R/opt/vkquake/id1"
cp vkQuake-$version/Quake/vkquake "$R/opt/vkquake/"
unzip -o -q -j quake106.zip QUAKE_SW/ID1/PAK0.PAK -d "$src"
echo "5906e5998fc3d896ddaf5e6a62e03abb  $src/PAK0.PAK" | md5sum -c --quiet
cp "$src/PAK0.PAK" "$R/opt/vkquake/id1/pak0.pak"
echo "$R/opt/vkquake"
