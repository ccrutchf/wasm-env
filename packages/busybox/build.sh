#!/usr/bin/env bash
# Builds BusyBox for wasm-env with the wasm64-linux-musl toolchain.
#
#   nix develop -c packages/busybox/build.sh
#
# Output: build/packages/busybox/busybox.wasm
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
here=$root/packages/busybox
out=$root/build/packages/busybox
: "${BUSYBOX_SRC:?run inside nix develop}"
[ -f "$root/build/sysroot/lib/libc.a" ] || "$root/toolchain/build.sh"

src=$out/src
rm -rf "$src" && mkdir -p "$src"
tar -xjf "$BUSYBOX_SRC" -C "$src" --strip-components=1

cc=$root/toolchain/bin/wasm64-linux-musl-cc
make_args=(-C "$src" CC="$cc" HOSTCC=cc AR=llvm-ar STRIP=true)

make "${make_args[@]}" allnoconfig >/dev/null
# Apply our options: replace each option's line (set or "is not set").
while IFS='=' read -r key value; do
	[[ $key =~ ^CONFIG_ ]] || continue
	sed -i "/^$key=/d; /^# $key is not set/d" "$src/.config"
	if [ "$value" = n ]; then
		echo "# $key is not set" >> "$src/.config"
	else
		echo "$key=$value" >> "$src/.config"
	fi
done < "$here/applets.txt"
make "${make_args[@]}" oldconfig < <(yes "") >/dev/null

make "${make_args[@]}" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)" busybox_unstripped
cp "$src/busybox_unstripped" "$out/busybox.wasm"
echo "built $out/busybox.wasm"
