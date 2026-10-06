#!/usr/bin/env bash
# Builds SDL3 and sdl2-compat for arm64 against the Ubuntu root
# (build/ubuntu, with libsdl2-dev's dependencies: Wayland, xkbcommon and
# ALSA headers) and installs them in its /opt/sdl3. SDL2 programs then run
# on SDL3 (LD_LIBRARY_PATH=/opt/sdl3/lib) and its Vulkan renderer
# (SDL_RENDER_DRIVER=vulkan): wasm-env's Vulkan driver, where Ubuntu's SDL2
# shows nothing on Wayland without OpenGL. SDL3 here has Wayland, Vulkan
# and ALSA only (no OpenGL, no X11).
#
#   nix develop -c packages/sdl3/build.sh
#
# Output: build/ubuntu/opt/sdl3. Needs network the first time (the release
# tarballs, checked against their SHA-256 sums).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
R=$root/build/ubuntu
src=$root/build/sdl3-src
out=$root/build/sdl3-out
: "${WASM_CC:?run inside nix develop}"
mkdir -p "$src" "$out"
cd "$src"
fetch() { # name, url, sha256
	[ -f "$1" ] || curl -sSfL -o "$1.part" "$2" && { [ -f "$1" ] || mv "$1.part" "$1"; }
	echo "$3  $1" | sha256sum -c --quiet
	[ -d "${1%.tar.gz}" ] || tar -xzf "$1"
}
fetch SDL3-3.4.18.tar.gz https://github.com/libsdl-org/SDL/releases/download/release-3.4.18/SDL3-3.4.18.tar.gz \
	9c75cf16330322c217dedd2e0609f1124f1b54b8633e763467b4684d0f4334a3
fetch sdl2-compat-2.32.74.tar.gz https://github.com/libsdl-org/sdl2-compat/releases/download/release-2.32.74/sdl2-compat-2.32.74.tar.gz \
	ec68abde77e2e459c8abc4f9587b8976b5d2a657b2bfda2fb3a079b0f8924588
# (The project's nixpkgs, as flake.lock pins it, for the build tools.)
store() { nix build --no-link --print-out-paths --inputs-from "$root" "nixpkgs#$1" | tail -1; }
export PATH=$(store cmake)/bin:$(store ninja)/bin:$(store pkg-config)/bin:$(store wayland-scanner.bin)/bin:$PATH
export PKG_CONFIG_SYSROOT_DIR=$R
export PKG_CONFIG_LIBDIR=$R/usr/lib/aarch64-linux-gnu/pkgconfig:$R/usr/share/pkgconfig
gcc_lib=$R/usr/lib/gcc/aarch64-linux-gnu/13
cat >"$out/toolchain.cmake" <<EOF
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR aarch64)
set(CMAKE_SYSROOT $R)
set(CMAKE_C_COMPILER $WASM_CC)
set(CMAKE_C_COMPILER_TARGET aarch64-linux-gnu)
set(CMAKE_C_FLAGS_INIT "-B$gcc_lib")
set(CMAKE_EXE_LINKER_FLAGS_INIT "-fuse-ld=lld -L$gcc_lib -L$R/usr/lib/aarch64-linux-gnu -Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "-fuse-ld=lld -L$gcc_lib -L$R/usr/lib/aarch64-linux-gnu")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
EOF
prefix=$out/install
rm -rf "$prefix"
cmake -S SDL3-3.4.18 -B "$out/sdl3" -G Ninja -DCMAKE_TOOLCHAIN_FILE="$out/toolchain.cmake" -DCMAKE_BUILD_TYPE=Release \
	-DCMAKE_INSTALL_PREFIX=/opt/sdl3 -DSDL_SHARED=ON -DSDL_STATIC=OFF -DSDL_TESTS=OFF -DSDL_EXAMPLES=OFF \
	-DSDL_WAYLAND=ON -DSDL_WAYLAND_LIBDECOR=OFF -DSDL_X11=OFF -DSDL_KMSDRM=OFF -DSDL_OFFSCREEN=ON \
	-DSDL_VULKAN=ON -DSDL_RENDER_VULKAN=ON -DSDL_OPENGL=OFF -DSDL_OPENGLES=OFF -DSDL_GPU=OFF \
	-DSDL_ALSA=ON -DALSA_INCLUDE_DIR="$R/usr/include" -DALSA_LIBRARY="$R/usr/lib/aarch64-linux-gnu/libasound.so" -DSDL_PULSEAUDIO=OFF -DSDL_PIPEWIRE=OFF -DSDL_JACK=OFF -DSDL_SNDIO=OFF \
	-DSDL_DBUS=OFF -DSDL_IBUS=OFF -DSDL_LIBUDEV=OFF -DSDL_HIDAPI=OFF -DSDL_CAMERA=OFF >"$out/sdl3.log"
cmake --build "$out/sdl3" >>"$out/sdl3.log" || { tail -30 "$out/sdl3.log"; exit 1; }
DESTDIR=$prefix cmake --install "$out/sdl3" >/dev/null
cmake -S sdl2-compat-2.32.74 -B "$out/sdl2-compat" -G Ninja -DCMAKE_TOOLCHAIN_FILE="$out/toolchain.cmake" -DCMAKE_BUILD_TYPE=Release \
	-DCMAKE_INSTALL_PREFIX=/opt/sdl3 -DSDL2COMPAT_TESTS=OFF -DSDL2COMPAT_X11=OFF \
	-DCMAKE_PREFIX_PATH="$prefix/opt/sdl3" >"$out/sdl2-compat.log"
cmake --build "$out/sdl2-compat" >>"$out/sdl2-compat.log" || { tail -30 "$out/sdl2-compat.log"; exit 1; }
DESTDIR=$prefix cmake --install "$out/sdl2-compat" >/dev/null
rm -rf "$R/opt/sdl3"
cp -a "$prefix/opt/sdl3" "$R/opt/sdl3"
echo "$R/opt/sdl3"
