#!/usr/bin/env bash
# Builds the wasm64-linux-musl sysroot: musl libc (with our overlay) and
# compiler-rt builtins, compiled for the wasm-env ABI (docs/abi.md).
#
#   nix develop -c toolchain/build.sh
#
# Output: build/sysroot/{include,lib}. Compile programs with
# toolchain/bin/wasm64-linux-musl-cc.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/build
sysroot=$out/sysroot
: "${WASM_CC:?run inside nix develop}" "${MUSL_SRC:?}" "${COMPILER_RT_SRC:?}"

# Flags shared by libc, compiler-rt and programs. Keep in sync with
# bin/wasm64-linux-musl-cc.
source "$root/toolchain/flags.sh"

jobs=$(nproc 2>/dev/null || sysctl -n hw.ncpu)

# --- musl ------------------------------------------------------------------
musl=$out/musl
rm -rf "$musl" && mkdir -p "$musl"
tar -xzf "$MUSL_SRC" -C "$musl" --strip-components=1

# arm64 headers used unchanged: wasm-env mirrors arm64 Linux layouts.
aarch64=$musl/arch/aarch64
mkdir -p "$musl/arch/wasm64/bits"
for f in bits/fcntl.h bits/float.h bits/signal.h bits/syscall.h.in bits/user.h kstat.h; do
	cp "$aarch64/$f" "$musl/arch/wasm64/$f"
done
cp -r "$root/toolchain/musl-overlay/." "$musl/"

# vfork returns twice, so on wasm64 it's a macro around setjmp, defined
# right after its declaration (see src/process/wasm64/vfork.c).
sed -i 's|^pid_t vfork(void);$|&\
#ifdef __wasm__\
#include <setjmp.h>\
jmp_buf *__wasm_env_vfork_prepare(void);\
pid_t __wasm_env_vfork_finish(int);\
#define vfork() __wasm_env_vfork_finish(setjmp(*__wasm_env_vfork_prepare()))\
#endif|' "$musl/include/unistd.h"
grep -q __wasm_env_vfork_finish "$musl/include/unistd.h"

cat > "$musl/config.mak" <<EOF
ARCH = wasm64
srcdir = .
prefix = $sysroot
exec_prefix = \$(prefix)
bindir = \$(exec_prefix)/bin
libdir = \$(prefix)/lib
includedir = \$(prefix)/include
syslibdir = /lib
CC = $WASM_CC
CFLAGS = ${WASM_ENV_CFLAGS[*]}
CFLAGS_AUTO = -O2 -pipe -ffunction-sections -fdata-sections -Wno-everything
CFLAGS_C99FSE = -std=c99 -nostdinc -ffreestanding -frounding-math
CFLAGS_MEMOPS = -fno-builtin
CFLAGS_NOSSP = -fno-stack-protector
CPPFLAGS =
LDFLAGS =
LIBCC =
AR = llvm-ar
RANLIB = llvm-ranlib
SHARED_LIBS =
ALL_TOOLS =
TOOL_LIBS =
EOF

# Static only: skip the PIE/dynamic-linking startup files (rcrt1, Scrt1).
make -C "$musl" -j"$jobs" CRT_LIBS=lib/crt1.o install >/dev/null
echo "musl installed to $sysroot"

# --- compiler-rt builtins --------------------------------------------------
# Only the generic C builtins: 128-bit integer and long double (IEEE quad)
# arithmetic, which clang emits calls to for wasm64.
rt=$out/compiler-rt
rm -rf "$rt" && mkdir -p "$rt"
upstream=$COMPILER_RT_SRC/compiler-rt/lib/builtins
[ -d "$upstream" ] || upstream=$COMPILER_RT_SRC/lib/builtins
builtins=$out/compiler-rt-src
rm -rf "$builtins" && cp -r "$upstream" "$builtins" && chmod -R u+w "$builtins"

# LLVM's WebAssembly backend expects the soft-float comparison functions
# (__eqtf2 etc.) to return int, but compiler-rt only uses int on arm64 and
# returns long on other 64-bit targets. Treat wasm like arm64.
sed -i 's/^#if defined(__aarch64__) || defined(__arm64ec__)$/& || defined(__wasm__)/' \
	"$builtins/fp_compare_impl.inc"
grep -q 'defined(__wasm__)' "$builtins/fp_compare_impl.inc"

# Skip files that need a real OS, an unwinder, or another architecture.
skip='^(atomic|clear_cache|crtbegin|crtend|emutls|enable_execute_stack|eprintf|gcc_personality_v0|os_version_check|trampoline_setup|cpu_model|aarch64|x86|i386|arm|riscv|ppc|mips|loongarch|hexagon|ve|.*xf.*|.*xc.*|.*bf.*|udivmodti4_fast)\b'
failed=()
for src in "$builtins"/*.c; do
	name=$(basename "$src" .c)
	[[ $name =~ $skip ]] && continue
	if ! "$WASM_CC" "${WASM_ENV_CFLAGS[@]}" -O2 -ffreestanding -nostdinc \
		-isystem "$sysroot/include" -I"$builtins" \
		-c "$src" -o "$rt/$name.o" 2>/dev/null; then
		failed+=("$name")
	fi
done
llvm-ar rcs "$sysroot/lib/libclang_rt.builtins.a" "$rt"/*.o
echo "compiler-rt builtins: $(ls "$rt" | wc -l) objects; skipped after errors: ${failed[*]:-none}"
