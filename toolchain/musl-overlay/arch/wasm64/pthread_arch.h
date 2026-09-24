// wasm-env: there is no thread-pointer register, so the thread pointer is
// an ordinary global, set by __set_thread_area (src/thread/wasm64).
// TLS sits below the thread pointer (the x86-64 layout).
extern hidden uintptr_t __wasm_env_tp;

static inline uintptr_t __get_tp()
{
	return __wasm_env_tp;
}

#define MC_PC pc
