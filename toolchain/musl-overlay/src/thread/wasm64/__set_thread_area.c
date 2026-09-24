// wasm-env: the thread pointer is a global (see arch/wasm64/pthread_arch.h).
#include "pthread_impl.h"

hidden uintptr_t __wasm_env_tp;

int __set_thread_area(void *p)
{
	__wasm_env_tp = (uintptr_t)p;
	return 0;
}
