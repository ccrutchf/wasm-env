// wasm-env: only used when a detached thread exits, and version 0 has no
// threads. Unmap and exit without switching stacks.
#include "pthread_impl.h"
#include "syscall.h"

void __unmapself(void *base, size_t size)
{
	__syscall(SYS_munmap, base, size);
	for (;;) __syscall(SYS_exit, 0);
}
