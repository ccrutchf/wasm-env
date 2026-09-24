// wasm-env: every syscall goes through one host import, using the
// generic (arm64) Linux syscall numbers and conventions. See docs/abi.md.
#define __SYSCALL_LL_E(x) (x)
#define __SYSCALL_LL_O(x) (x)

long __wasm_env_syscall(long, long, long, long, long, long, long)
	__attribute__((__import_module__("linux"), __import_name__("syscall")));

static inline long __syscall0(long n)
{
	return __wasm_env_syscall(n, 0, 0, 0, 0, 0, 0);
}

static inline long __syscall1(long n, long a)
{
	return __wasm_env_syscall(n, a, 0, 0, 0, 0, 0);
}

static inline long __syscall2(long n, long a, long b)
{
	return __wasm_env_syscall(n, a, b, 0, 0, 0, 0);
}

static inline long __syscall3(long n, long a, long b, long c)
{
	return __wasm_env_syscall(n, a, b, c, 0, 0, 0);
}

static inline long __syscall4(long n, long a, long b, long c, long d)
{
	return __wasm_env_syscall(n, a, b, c, d, 0, 0);
}

static inline long __syscall5(long n, long a, long b, long c, long d, long e)
{
	return __wasm_env_syscall(n, a, b, c, d, e, 0);
}

static inline long __syscall6(long n, long a, long b, long c, long d, long e, long f)
{
	return __wasm_env_syscall(n, a, b, c, d, e, f);
}

#define IPC_64 0
