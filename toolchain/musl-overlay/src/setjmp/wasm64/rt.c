// wasm-env: runtime support for setjmp/longjmp via WebAssembly exception
// handling. With -mllvm -wasm-enable-sjlj, clang rewrites every setjmp
// call site into calls to __wasm_setjmp/__wasm_setjmp_test, and longjmp
// into __wasm_longjmp, which throws the C tag __c_longjmp. The catching
// function asks __wasm_setjmp_test whether the jmp_buf belongs to one of
// its own setjmp calls, and if not, rethrows.
#include <setjmp.h>
#include <stdint.h>

struct jmp_buf_impl {
	void *func_invocation_id;
	uint32_t label;
	// Payload thrown by longjmp; read by the compiler-generated catch.
	struct {
		void *env;
		int val;
	} arg;
};

void __wasm_setjmp(void *env, uint32_t label, void *func_invocation_id)
{
	struct jmp_buf_impl *jb = env;
	jb->func_invocation_id = func_invocation_id;
	jb->label = label;
}

uint32_t __wasm_setjmp_test(void *env, void *func_invocation_id)
{
	struct jmp_buf_impl *jb = env;
	return jb->func_invocation_id == func_invocation_id ? jb->label : 0;
}

_Noreturn void __wasm_longjmp(void *env, int val)
{
	struct jmp_buf_impl *jb = env;
	jb->arg.env = env;
	jb->arg.val = val ? val : 1;
	// Tag 1 is the C longjmp tag (__c_longjmp).
	__builtin_wasm_throw(1, &jb->arg);
}

// For code that calls longjmp through a pointer instead of by name.
_Noreturn void longjmp(jmp_buf env, int val)
{
	__wasm_longjmp(env, val);
}

_Noreturn void _longjmp(jmp_buf env, int val)
{
	__wasm_longjmp(env, val);
}
