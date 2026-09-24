// wasm-env: vfork on WebAssembly (docs/abi.md, "Processes").
//
// vfork returns twice, so unistd.h defines it as a macro around setjmp:
//
//   vfork() == __wasm_env_vfork_finish(setjmp(*__wasm_env_vfork_prepare()))
//
// The first return (0) enters the child: clone(CLONE_VM|CLONE_VFORK) tells
// the runtime to give the child its own pid and a copy of the process
// state, while it keeps running in the parent's memory, as a real vfork
// child does. When the child calls execve or _exit, the runtime starts the
// new program (or records the exit), restores the parent's state and
// returns the child's pid; libc then longjmps back to the vfork call site,
// where setjmp returns the pid to the parent.
#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <unistd.h>
#include "syscall.h"
#include "atomic.h"

#define CLONE_VM 0x00000100
#define CLONE_VFORK 0x00004000
#define MAX_DEPTH 8

static jmp_buf frames[MAX_DEPTH];
hidden int __wasm_env_vfork_depth;

jmp_buf *__wasm_env_vfork_prepare(void)
{
	if (__wasm_env_vfork_depth == MAX_DEPTH) a_crash();
	return &frames[__wasm_env_vfork_depth++];
}

pid_t __wasm_env_vfork_finish(int v)
{
	if (v) {
		// Back in the parent: the child exec'd or exited.
		__wasm_env_vfork_depth--;
		return v;
	}
	long r = __syscall(SYS_clone, CLONE_VM | CLONE_VFORK | SIGCHLD, 0);
	if (r < 0) {
		__wasm_env_vfork_depth--;
		errno = -r;
		return -1;
	}
	return 0; // in the child
}

// Called by execve and _Exit when the runtime says the vfork child is done.
hidden _Noreturn void __wasm_env_vfork_return(long pid)
{
	longjmp(frames[__wasm_env_vfork_depth - 1], pid);
}

// For code that calls vfork by name rather than through the macro (e.g.
// through a function pointer). It can't return twice, so it can't work.
#undef vfork
pid_t vfork(void)
{
	errno = ENOSYS;
	return -1;
}
