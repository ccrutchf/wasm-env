// The program break: brk grows (zero-filled) within its region, refuses to
// move past it (returning the old break, as Linux does), and zero-fills
// again after shrinking and regrowing. glibc's malloc relies on it. musl's
// sbrk only reports the break, so this uses the raw syscall.
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

static char *brk_to(char *p) { return (char *)syscall(SYS_brk, (long)p); }

int main(void)
{
	char *b0 = brk_to(0);
	char *b1 = brk_to(b0 + (1 << 20));
	printf("grew: %d\n", b1 == b0 + (1 << 20));
	printf("zero-filled: %d\n", b0[0] == 0 && b0[(1 << 20) - 1] == 0);
	b0[12345] = 0x5a;
	printf("writable: %d\n", b0[12345] == 0x5a);
	printf("past the limit: %s\n", brk_to(b1 + (1L << 40)) == b1 ? "unchanged" : "moved");
	brk_to(b0 + 4096);
	brk_to(b1);
	printf("zeroed after regrowing: %d\n", b0[12345] == 0);
	return 0;
}
