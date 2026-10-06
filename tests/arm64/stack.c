// The main thread's stack is Linux's 8 MiB (and RLIMIT_STACK says so): 6
// MiB of it in one array, every byte written, then deep recursion. (With
// less, the array ran over the support code's data below it.)
#include <stdio.h>
#include <sys/resource.h>

static long deep(long n)
{
	volatile char pad[48];
	pad[0] = (char)n;
	return n ? deep(n - 1) + (pad[0] & 1) : 0;
}

int main(void)
{
	struct rlimit r;
	getrlimit(RLIMIT_STACK, &r);
	printf("RLIMIT_STACK: %lu MiB\n", (unsigned long)(r.rlim_cur >> 20));
	volatile char big[6 << 20];
	for (unsigned long i = 0; i < sizeof big; i++)
		big[i] = (char)(i >> 12);
	long sum = 0;
	for (unsigned long i = 0; i < sizeof big; i += 4096)
		sum += big[i];
	printf("6 MiB on the stack: %ld\n", sum);
	printf("recursion 10000 deep: %ld\n", deep(10000));
	return 0;
}
