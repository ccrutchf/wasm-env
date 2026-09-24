// wasm-env: used when the program defines main(int, char **), which clang
// names __main_argc_argv. See arch/wasm64/crt_arch.h.
int __main_argc_argv(int, char **);

extern int __wasm_env_argc;
extern char **__wasm_env_argv;

__attribute__((__weak__)) int __main_void(void)
{
	return __main_argc_argv(__wasm_env_argc, __wasm_env_argv);
}
