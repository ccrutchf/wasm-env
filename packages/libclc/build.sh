#!/usr/bin/env bash
# Builds LLVM's libclc, OpenCL's builtins written in OpenCL C, for the GPU
# service (gpu/src/opencl.rs): accurate math (sin, exp, pow, acos...) where
# a GPU's own, in WGSL, is too loose for OpenCL. Its clspv target is the one
# made for this: 32-bit spir, compute shaders, math done in software. Its
# functions are marked to be inlined (the WGSL compiler wants kernels whole).
#
#   nix develop -c packages/libclc/build.sh
#
# Output: build/libclc/clspv--.bc (LLVM bitcode: the same on every host).
# Needs network the first time (LLVM's sources: libclc and its cmake files).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
version=21.1.8
src=$root/build/libclc-src
out=$root/build/libclc
mkdir -p "$src" "$out"
cd "$src"
for p in libclc cmake; do
	[ -f $p-$version.src.tar.xz ] || curl -sSfLO https://github.com/llvm/llvm-project/releases/download/llvmorg-$version/$p-$version.src.tar.xz
	[ -d $p-$version.src ] || tar -xf $p-$version.src.tar.xz
done
ln -sfn cmake-$version.src cmake
# OpenCL's zeros: signed ones in sinpi, cospi and tanpi; tgamma's underflow
# for large negative arguments (the CTS checks them).
rm -rf libclc && cp -r libclc-$version.src libclc && patch -s -d libclc -p1 < "$root/packages/libclc/signed-zeros.patch"
store() { nix build --no-link --print-out-paths "nixpkgs#$1" | tail -1; }
llvm=$(store llvmPackages_21.llvm)
clang=$(store llvmPackages_21.clang-unwrapped)
export PATH=$(store ninja)/bin:$(store python3)/bin:$(store cmake)/bin:$llvm/bin:$clang/bin:$PATH
cmake -S libclc -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DLIBCLC_TARGETS_TO_BUILD=clspv-- \
	-DLLVM_DIR="$(store llvmPackages_21.llvm.dev)/lib/cmake/llvm" -DLLVM_TOOLS_BINARY_DIR="$llvm/bin" -DLLVM_TOOL_clang="$clang/bin/clang" >/dev/null
ninja -C build >/dev/null
# Inlined into kernels: no noinline or no-builtins (which stop the inliner),
# and alwaysinline on its functions (log and the like are big).
llvm-dis build/clspv--.bc -o - |
	sed -E '/^attributes #/{s/ noinline//g; s/ "no-builtins"//g; /"frame-pointer"/s/^(attributes #[0-9]+ = \{ )/\1alwaysinline /}' |
	llvm-as -o "$out/clspv--.bc"
echo "$out/clspv--.bc"
