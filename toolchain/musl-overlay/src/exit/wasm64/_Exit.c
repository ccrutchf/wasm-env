// wasm-env: in a vfork child, exit_group records the child's exit and
// returns its pid (see src/process/wasm64/vfork.c); jump back to the parent.
#include <stdlib.h>
#include "syscall.h"

hidden extern int __wasm_env_vfork_depth;
hidden _Noreturn void __wasm_env_vfork_return(long);

_Noreturn void _Exit(int ec)
{
	long r = __syscall(SYS_exit_group, ec);
	if (r > 0 && __wasm_env_vfork_depth) __wasm_env_vfork_return(r);
	for (;;) __syscall(SYS_exit, ec);
}
