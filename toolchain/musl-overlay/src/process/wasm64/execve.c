// wasm-env: in a vfork child, a successful execve returns the child's pid
// (see vfork.c); jump back to the parent.
#include <unistd.h>
#include "syscall.h"

hidden extern int __wasm_env_vfork_depth;
hidden _Noreturn void __wasm_env_vfork_return(long);

int execve(const char *path, char *const argv[], char *const envp[])
{
	long r = __syscall(SYS_execve, path, argv, envp);
	if (r > 0 && __wasm_env_vfork_depth) __wasm_env_vfork_return(r);
	return __syscall_ret(r);
}
