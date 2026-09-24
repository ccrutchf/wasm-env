# Compiler flags for the wasm-env ABI (docs/abi.md), shared by
# toolchain/build.sh and toolchain/bin/wasm64-linux-musl-cc.
WASM_ENV_CFLAGS=(
	--target=wasm64-unknown-unknown
	# WebAssembly features allowed by ABI version 0.
	-mbulk-memory -mmutable-globals -msign-ext -mnontrapping-fptoint
	-mmultivalue -mreference-types -msimd128 -mexception-handling -mtail-call
	# setjmp/longjmp via WebAssembly exception handling (exnref form).
	-mllvm -wasm-enable-sjlj -mllvm -wasm-use-legacy-eh=false
	# clang has no wasm64-linux target; present as Linux to programs.
	-D__linux__ -D__linux -D__unix__ -D__unix
)

WASM_ENV_LDFLAGS=(
	-Wl,--export=__stack_pointer
	-Wl,--export=__heap_base
	-Wl,-z,stack-size=8388608
)
