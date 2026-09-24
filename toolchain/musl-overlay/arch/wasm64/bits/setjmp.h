// wasm-env: setjmp/longjmp are implemented with WebAssembly exception
// handling (src/setjmp/wasm64/rt.c). The buffer only needs a few words;
// the size matches aarch64 to leave room.
typedef unsigned long __jmp_buf[22];
