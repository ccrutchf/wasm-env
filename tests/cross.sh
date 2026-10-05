#!/usr/bin/env bash
# Checks that wasm-env builds for the other hosts, from Linux: macOS on
# Apple silicon (aarch64-apple-darwin) and Windows (x86_64-pc-windows-gnu).
# With --build, also builds Windows executables (MinGW's gcc links them),
# which tests can run under Wine: WASM_ENV_RUNTIME=build/wine-wasm-env
# tests/m1.sh (docs/hosts.md). With --unit [filter], runs the runtime's and
# the GPU service's unit tests under Wine.
#
# Needs network the first time: a Rust toolchain with those targets (in
# build/rustup), zig (C headers for both targets), MinGW's gcc, and Wine.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
toolchain=1.98.1
export RUSTUP_HOME=$root/build/rustup CARGO_HOME=$root/build/cargo-home CARGO_TARGET_DIR=$root/build/target-cross
store() { nix build --no-link --print-out-paths --inputs-from "$root" "nixpkgs#$1" | tail -1; }
rustup=$(store rustup)/bin/rustup
"$rustup" toolchain install "$toolchain" --profile minimal --target aarch64-apple-darwin --target x86_64-pc-windows-gnu >/dev/null 2>&1
zig=$(store zig)
headers=$zig/lib/zig
mingw=$(store pkgsCross.mingwW64.stdenv.cc)
pthreads=$(store pkgsCross.mingwW64.windows.pthreads)
# That toolchain's cargo and rustc (not the dev shell's, which has no
# other targets).
cargo=$("$rustup" which --toolchain "$toolchain" cargo)
export RUSTC=$("$rustup" which --toolchain "$toolchain" rustc)

# zig as a C compiler for macOS (crates with C code).
wrap=$root/build/cross-wrap
mkdir -p "$wrap"
cat >"$wrap/cc-aarch64-macos" <<EOF
#!/usr/bin/env bash
args=()
for a in "\$@"; do case "\$a" in --target=*|-target) ;; *) args+=("\$a");; esac; done
exec $zig/bin/zig cc -target aarch64-macos "\${args[@]}"
EOF
cat >"$wrap/ar" <<EOF
#!/usr/bin/env bash
exec $zig/bin/zig ar "\$@"
EOF
chmod +x "$wrap"/*
export CC_aarch64_apple_darwin=$wrap/cc-aarch64-macos AR_aarch64_apple_darwin=$wrap/ar
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_apple_darwin="--target=aarch64-apple-darwin -isystem $headers/include -isystem $headers/libc/include/any-darwin-any"
export PATH=$mingw/bin:$PATH
export CC_x86_64_pc_windows_gnu=$mingw/bin/x86_64-w64-mingw32-gcc AR_x86_64_pc_windows_gnu=$mingw/bin/x86_64-w64-mingw32-ar
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=$mingw/bin/x86_64-w64-mingw32-gcc
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-L native=$pthreads/lib"
export BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu="--target=x86_64-pc-windows-gnu -isystem $headers/include -isystem $headers/libc/include/any-windows-any"

status=0
for target in aarch64-apple-darwin x86_64-pc-windows-gnu; do
	out=$("$cargo" check -q --release --target "$target" --workspace --all-targets 2>&1 || true)
	if grep -E '^(error|warning)' <<<"$out"; then
		echo "FAIL $target: errors or warnings"
		status=1
	else
		echo "ok   $target"
	fi
done
if [ "${1:-}" = --build ]; then
	"$cargo" build -q --release --target x86_64-pc-windows-gnu -p wasm-env -p wasm-env-translate --features wasm-env-translate/lift
	wine=$(store wineWow64Packages.stable)
	cat >"$root/build/wine-wasm-env" <<EOF
#!/usr/bin/env bash
# The Windows build of wasm-env, under Wine (tests: WASM_ENV_RUNTIME=\$0).
export WINEPREFIX=$root/build/wineprefix WINEDEBUG=-all WINEDLLOVERRIDES="mscoree,mshtml="
exec $wine/bin/wine $CARGO_TARGET_DIR/x86_64-pc-windows-gnu/release/wasm-env.exe "\$@"
EOF
	chmod +x "$root/build/wine-wasm-env"
	# A Wine prefix, made without Mono or Gecko (their installers would ask).
	[ -d "$root/build/wineprefix" ] || WINEPREFIX=$root/build/wineprefix WINEDEBUG=-all WINEDLLOVERRIDES="mscoree,mshtml=" "$wine/bin/wineboot" --init >/dev/null 2>&1
	echo "built: build/wine-wasm-env runs the Windows build under Wine"
fi
if [ "${1:-}" = --unit ]; then
	# The runtime's and the GPU service's unit tests, built for Windows and
	# run under Wine (OpenCL kernels compiled by the Windows toolchain's
	# clang, in build/win-toolchain, if it's there).
	wine=$(store wineWow64Packages.stable)
	export WINEPREFIX=$root/build/wineprefix WINEDEBUG=-all WINEDLLOVERRIDES="mscoree,mshtml="
	[ -d "$WINEPREFIX" ] || "$wine/bin/wineboot" --init >/dev/null 2>&1
	if [ -e build/win-toolchain/bin/clang.exe ]; then
		export WASM_CC="Z:$(pwd | tr / '\\')\\build\\win-toolchain\\bin\\clang.exe"
	fi
	for package in wasm-env wasm-env-gpu; do
		exe=$("$cargo" test -q --release --target x86_64-pc-windows-gnu -p $package --no-run --message-format=json 2>/dev/null |
			grep -o '"executable":"[^"]*"' | cut -d'"' -f4 | head -1)
		"$wine/bin/wine" "$exe" ${2:+"$2"} || status=1
	done
fi
exit $status
