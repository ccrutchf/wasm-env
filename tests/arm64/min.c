// The smallest arm64 test program for the translator: no libc, just raw
// syscalls, a loop, a call and some arithmetic.
//   aarch64-unknown-linux-musl-gcc -O2 -static -nostdlib -o min min.c
static long sys(long n, long a, long b, long c)
{
	register long x8 __asm__("x8") = n;
	register long x0 __asm__("x0") = a;
	register long x1 __asm__("x1") = b;
	register long x2 __asm__("x2") = c;
	__asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2) : "memory");
	return x0;
}

static unsigned long fib(unsigned n)
{
	return n < 2 ? n : fib(n - 1) + fib(n - 2);
}

static char *utoa(unsigned long v, char *end)
{
	*--end = '\n';
	do *--end = '0' + v % 10; while (v /= 10);
	return end;
}

void _start(void)
{
	static const char msg[] = "hello from translated arm64\n";
	char buf[32];
	sys(64, 1, (long)msg, sizeof msg - 1);   // write
	char *s = utoa(fib(20), buf + sizeof buf);
	sys(64, 1, (long)s, buf + sizeof buf - s);
	sys(94, 3, 0, 0);                        // exit_group(3)
	for (;;);
}
