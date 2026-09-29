// Code generated at run time, as a JIT does: instructions written into an
// anonymous mapping, made executable, and called. Two kernels in one
// buffer entered separately, and one entered in its middle; a kernel that
// calls back into compiled code (and is returned to); code rewritten in
// place after making it writable again, and in a writable and executable
// mapping with only cache maintenance; and new code in a fresh mapping
// after the old one is unmapped.
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define LEN 16384

static uint32_t *map_code(void)
{
	void *p = mmap(0, LEN, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	return p == MAP_FAILED ? 0 : p;
}

// Copies `n` instructions to `at` (a word index into the buffer).
static void emit(uint32_t *buf, int at, const uint32_t *code, int n)
{
	memcpy(buf + at, code, n * 4);
}

static void make_executable(uint32_t *buf)
{
	__builtin___clear_cache((char *)buf, (char *)buf + LEN);
	mprotect(buf, LEN, PROT_READ | PROT_EXEC);
}

static long twice_plus(long x) { return 2 * x + 100; }

int main(void)
{
	static const uint32_t times3_plus7[] = {
		0x8b000400, // add x0, x0, x0, lsl #1
		0x91001c00, // add x0, x0, #7
		0xd65f03c0, // ret
	};
	static const uint32_t sum_to_n[] = {
		0xd2800001, // mov x1, #0
		0xb4000080, // loop: cbz x0, done
		0x8b000021, // add x1, x1, x0
		0xd1000400, // sub x0, x0, #1
		0x17fffffd, // b loop
		0xaa0103e0, // done: mov x0, x1
		0xd65f03c0, // ret
	};
	static const uint32_t call_then_inc[] = {
		0xa9bf7bfd, // stp x29, x30, [sp, #-16]!
		0xd63f0020, // blr x1
		0x91000400, // add x0, x0, #1
		0xa8c17bfd, // ldp x29, x30, [sp], #16
		0xd65f03c0, // ret
	};
	static const uint32_t times5[] = {
		0x8b000800, // add x0, x0, x0, lsl #2
		0xd65f03c0, // ret
	};
	static const uint32_t minus1[] = {
		0xd1000400, // sub x0, x0, #1
		0xd65f03c0, // ret
	};

	uint32_t *buf = map_code();
	if (!buf)
		return 1;
	emit(buf, 0, times3_plus7, 3);
	emit(buf, 64, sum_to_n, 7);
	emit(buf, 128, call_then_inc, 5);
	make_executable(buf);
	long (*f)(long) = (long (*)(long))buf;
	long (*sum)(long) = (long (*)(long))(buf + 64);
	long (*call)(long, long (*)(long)) = (long (*)(long, long (*)(long)))(buf + 128);
	printf("3x+7: %ld %ld\n", f(5), f(-2));
	printf("sum to n: %ld %ld\n", sum(10), sum(1000));
	printf("call back: %ld\n", call(21, twice_plus));
	long total = 0;
	for (long i = 0; i < 100000; i++)
		total += f(i) & 7;
	printf("many calls: %ld\n", total);
	// Entered mid-kernel (no branch or code pointer shows that address).
	long (*g)(long) = (long (*)(long))(buf + 1);
	printf("mid entry: %ld\n", g(5));

	// Rewritten in place: writable, changed, executable again.
	mprotect(buf, LEN, PROT_READ | PROT_WRITE);
	emit(buf, 0, times5, 2);
	make_executable(buf);
	printf("rewritten: %ld\n", f(9));

	// Writable and executable throughout: rewritten in place, with only the
	// cache maintenance the architecture requires (no mprotect).
	uint32_t *rwx = mmap(0, LEN, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (rwx == MAP_FAILED)
		return 1;
	long (*h)(long) = (long (*)(long))rwx;
	emit(rwx, 0, times3_plus7, 3);
	__builtin___clear_cache((char *)rwx, (char *)rwx + 12);
	printf("rwx: %ld", h(10));
	emit(rwx, 0, times5, 2);
	__builtin___clear_cache((char *)rwx, (char *)rwx + 8);
	printf(" then %ld\n", h(10));
	munmap(rwx, LEN);

	// Unmapped; new code in a new mapping (often at the same address).
	munmap(buf, LEN);
	buf = map_code();
	if (!buf)
		return 1;
	emit(buf, 0, minus1, 2);
	make_executable(buf);
	f = (long (*)(long))buf;
	printf("new mapping: %ld\n", f(9));
	return 0;
}
