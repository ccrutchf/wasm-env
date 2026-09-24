// Differential fixture: control flow. Jump tables, recursion, calls
// through pointers, tail calls, loops, setjmp/longjmp, and exit status.
#include <setjmp.h>
#include <stdio.h>
#include <stdlib.h>

static volatile int seed = 7;

__attribute__((noinline)) static int classify(int x)
{
	// Dense cases: gcc emits a jump table (an indirect branch).
	switch (x) {
	case 0: return 10;
	case 1: return 11;
	case 2: return 13;
	case 3: return 17;
	case 4: return 19;
	case 5: return 23;
	case 6: return 29;
	case 7: return 31;
	case 8: return 37;
	default: return -1;
	}
}

__attribute__((noinline)) static unsigned long ackermann(unsigned long m, unsigned long n)
{
	if (m == 0) return n + 1;
	if (n == 0) return ackermann(m - 1, 1);
	return ackermann(m - 1, ackermann(m, n - 1));
}

static int twice(int x) { return 2 * x; }
static int square(int x) { return x * x; }
static int (*volatile ops[])(int) = { twice, square };

__attribute__((noinline)) static int apply(int (*f)(int), int x)
{
	return f(x); // an indirect tail call at -O2
}

__attribute__((noinline)) static int even(int n);
__attribute__((noinline)) static int odd(int n) { return n == 0 ? 0 : even(n - 1); }
__attribute__((noinline)) static int even(int n) { return n == 0 ? 1 : odd(n - 1); }

static jmp_buf env;

// Recurses until n reaches 0, then longjmps out: gcc can't see that it ends.
#pragma GCC diagnostic ignored "-Winfinite-recursion"
__attribute__((noinline)) static void deep(int n)
{
	if (n <= 0) longjmp(env, 99);
	deep(n - 1);
}

int main(void)
{
	for (int i = -1; i < 11; i++)
		printf("classify(%d) = %d\n", i, classify(i));
	printf("ackermann(2, 3) = %lu\n", ackermann(2, 3));
	printf("ackermann(3, 3) = %lu\n", ackermann(3, 3));
	for (int i = 0; i < 2; i++)
		printf("apply(ops[%d], %d) = %d\n", i, seed, apply(ops[i], seed));
	printf("even(10001) = %d\n", even(10001));

	unsigned long sum = 0;
	for (int i = 0; i < 100000; i++)
		sum += (unsigned long)i * (unsigned)seed ^ (sum >> 3);
	printf("loop sum = %lu\n", sum);

	int r = setjmp(env);
	if (r == 0)
		deep(1000);
	printf("longjmp returned %d\n", r);

	fflush(stdout);
	exit(seed + 30);
}
