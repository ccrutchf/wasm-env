// futex in a single-threaded process: wake finds no waiters; wait returns
// EAGAIN when the word changed, times out (relative, or absolute with
// FUTEX_WAIT_BITSET), and otherwise wakes spuriously. glibc needs these.
// (syscall() is variadic and reads longs: arguments are cast to long.)
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

enum { WAIT = 0, WAKE = 1, WAIT_BITSET = 9, PRIVATE = 128 };

static const char *err(long r) { return r == 0 ? "ok" : errno == EAGAIN ? "EAGAIN" : errno == ETIMEDOUT ? "ETIMEDOUT" : errno == EINTR ? "EINTR" : "other"; }

int main(void)
{
	uint32_t w = 0;
	long r = syscall(SYS_futex, &w, (long)(WAIT | PRIVATE), 1L, 0L, 0L, 0L);
	printf("wait on a changed word: %s\n", err(r));
	struct timespec t = { 0, 20000000 };
	r = syscall(SYS_futex, &w, (long)(WAIT | PRIVATE), 0L, &t, 0L, 0L);
	printf("wait with a timeout: %s\n", err(r));
	struct timespec deadline;
	clock_gettime(CLOCK_MONOTONIC, &deadline);
	deadline.tv_nsec += 20000000;
	if (deadline.tv_nsec >= 1000000000) deadline.tv_sec++, deadline.tv_nsec -= 1000000000;
	r = syscall(SYS_futex, &w, (long)(WAIT_BITSET | PRIVATE), 0L, &deadline, 0L, 0xffffffffL);
	printf("wait_bitset with a deadline: %s\n", err(r));
	struct timespec after;
	clock_gettime(CLOCK_MONOTONIC, &after);
	printf("deadline respected: %d\n", after.tv_sec > deadline.tv_sec || (after.tv_sec == deadline.tv_sec && after.tv_nsec >= deadline.tv_nsec));
	r = syscall(SYS_futex, &w, (long)(WAKE | PRIVATE), 1L, 0L, 0L, 0L);
	printf("wake: %ld\n", r);
	return 0;
}
