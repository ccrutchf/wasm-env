// wasm-env: included by crt/crt1.c. The runtime calls _start(sp) with a
// pointer to the Linux-layout initial process stack (docs/abi.md).
hidden void _start_c(long *p);

__attribute__((__export_name__("_start")))
void _start(long *p)
{
	_start_c(p);
}

// WebAssembly has no .init_array; wasm-ld generates __wasm_call_ctors
// instead. musl calls _init after libc is initialized and before main.
void __wasm_call_ctors(void);

void _init(void)
{
	__wasm_call_ctors();
}

// ABI version, as a LEB128 integer in the "wasm-env.abi" custom section.
__attribute__((__section__(".custom_section.wasm-env.abi"), __used__))
static const unsigned char __wasm_env_abi_version[] = { 0 };

// clang renames main on WebAssembly: main(int, char **) becomes
// __main_argc_argv and main(void) becomes __main_void. musl calls main
// through a pointer as main(argc, argv, envp), and an indirect call with
// the wrong signature traps, so crt1 passes this adapter instead.
//
// The adapter always calls __main_void (a strong reference). A program
// with main(void) defines it; otherwise libc's __main_void
// (src/env/wasm64/__main_void.c) calls __main_argc_argv, again by strong
// reference, so main is found even inside a static library.
int __main_void(void);

int __wasm_env_argc;
char **__wasm_env_argv;

static int __wasm_env_main(int argc, char **argv, char **envp)
{
	__wasm_env_argc = argc;
	__wasm_env_argv = argv;
	return __main_void();
}

// crt1.c declares and passes "main"; redirect it to the adapter.
#define main __wasm_env_main
