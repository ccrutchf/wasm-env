// Calls and returns, which the translator maps onto wasm calls: recursion
// past its nesting limit, longjmp out of nested calls (again and again, so
// stranded frames must stay bounded), context switches between stacks,
// indirect and tail calls.
#include <setjmp.h>
#include <stdio.h>
#ifdef __GLIBC__
#include <ucontext.h>
#endif

__attribute__((noinline)) static long depth_sum(long n)
{
	if (n == 0)
		return 0;
	return n + depth_sum(n - 1) * 1; // not a tail call
}

static jmp_buf env;
__attribute__((noinline)) static void deep_throw(int n, int code)
{
	if (n == 0)
		longjmp(env, code);
	deep_throw(n - 1, code);
	__asm__ volatile("" ::: "memory");
}

#ifdef __GLIBC__
static ucontext_t main_ctx, co_ctx;
static char co_stack[1 << 16];
static volatile long co_value;
static void coroutine(void)
{
	for (long i = 0;; i++) {
		co_value = i * i;
		swapcontext(&co_ctx, &main_ctx);
	}
}
#endif

__attribute__((noinline)) static long add1(long x) { return x + 1; }
__attribute__((noinline)) static long twice(long x) { return 2 * x; }
static long (*volatile ops[2])(long) = { add1, twice };
__attribute__((noinline)) static long tail_to(long x) { return ops[x & 1](x); } // sibling call

int main(void)
{
	printf("recursion 1000: %ld\n", depth_sum(1000));
	printf("recursion 100000: %ld\n", depth_sum(100000));

	long caught = 0;
	for (int i = 0; i < 100000; i++) {
		int code = setjmp(env);
		if (code == 0)
			deep_throw(i % 50, i % 7 + 1);
		else
			caught += code;
	}
	printf("longjmp caught %ld\n", caught);
	printf("recursion after longjmps: %ld\n", depth_sum(20000));

#ifdef __GLIBC__
	getcontext(&co_ctx);
	co_ctx.uc_stack.ss_sp = co_stack;
	co_ctx.uc_stack.ss_size = sizeof co_stack;
	co_ctx.uc_link = 0;
	makecontext(&co_ctx, coroutine, 0);
	long sum = 0;
	for (int i = 0; i < 50000; i++) {
		swapcontext(&main_ctx, &co_ctx);
		sum += co_value;
	}
	printf("coroutine sum %ld\n", sum);
#endif

	long t = 0;
	for (long i = 0; i < 1000; i++)
		t += tail_to(i);
	printf("indirect/tail calls %ld\n", t);
	printf("recursion at the end: %ld\n", depth_sum(50000));
	return 0;
}
