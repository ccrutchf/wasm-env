// Freestanding main dynamically linked against dynlib.so. Cross-object
// calls (lib_add/lib_sum) go through the PLT/GOT; cross-object data
// (lib_value) through the GOT. No libc, so the test isolates the dynamic
// linker's job (which we replace by prelinking at translation time).
extern int lib_add(int, int);
extern int lib_sum(void);
extern long lib_value;

static long sys(long n, long a, long b, long c)
{
	register long x8 __asm__("x8") = n;
	register long x0 __asm__("x0") = a;
	register long x1 __asm__("x1") = b;
	register long x2 __asm__("x2") = c;
	__asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2) : "memory");
	return x0;
}

static char *utoa(unsigned long v, char *end)
{
	*--end = '\n';
	do *--end = '0' + v % 10; while (v /= 10);
	return end;
}

void _start(void)
{
	char buf[32];
	unsigned long r = (unsigned long)lib_add(20, 22) + lib_sum() + (unsigned long)lib_value;
	char *s = utoa(r, buf + sizeof buf);
	sys(64, 1, (long)s, buf + sizeof buf - s); // write
	sys(94, 0, 0, 0);                          // exit_group
}
