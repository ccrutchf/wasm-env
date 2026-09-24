// ARMv8.1 LSE atomics (ldadd, ldclr, ldeor, ldset, ld{s,u}{max,min}, swp,
// cas; byte/halfword/word/doubleword; acquire/release variants; the st*
// aliases), as compilers emit for C11 atomics with -march=armv8.1-a.
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>

#define LD(op, suffix, reg, type, mem, v)                                              \
	({                                                                            \
		type old_;                                                            \
		__asm__ volatile("ld" #op suffix " %" reg "2, %" reg "0, [%1]"        \
		                 : "=r"(old_) : "r"(&mem), "r"(v) : "memory");        \
		old_;                                                                 \
	})

int main(void)
{
	uint64_t x = 0x0f0f0f0f00000005ull;
	uint32_t w = 0x80000001u;
	uint16_t h = 0xfff0;
	uint8_t b = 0x7f;

	printf("ldaddal x: old=%llx ", (unsigned long long)LD(add, "al", "x", uint64_t, x, 10ull));
	printf("now=%llx\n", (unsigned long long)x);
	printf("ldclra w: old=%x ", LD(clr, "a", "w", uint32_t, w, 0x1u));
	printf("now=%x\n", w);
	printf("ldeorl w: old=%x ", LD(eor, "l", "w", uint32_t, w, 0xffffffffu));
	printf("now=%x\n", w);
	printf("ldset x: old=%llx ", (unsigned long long)LD(set, "", "x", uint64_t, x, 0xf000000000000000ull));
	printf("now=%llx\n", (unsigned long long)x);
	printf("ldaddh: old=%x ", LD(add, "h", "w", uint32_t, h, 0x20u));
	printf("now=%x\n", h);
	printf("ldsmaxb: old=%x ", LD(smax, "b", "w", uint32_t, b, 0x80u));
	printf("now=%x\n", b);
	printf("ldumaxb: old=%x ", LD(umax, "b", "w", uint32_t, b, 0x80u));
	printf("now=%x\n", b);
	printf("ldsmin w: old=%x ", LD(smin, "", "w", uint32_t, w, 5u));
	printf("now=%x\n", w);
	printf("lduminal x: old=%llx ", (unsigned long long)LD(umin, "al", "x", uint64_t, x, 3ull));
	printf("now=%llx\n", (unsigned long long)x);

	uint64_t old;
	__asm__ volatile("swpal %2, %0, [%1]" : "=r"(old) : "r"(&x), "r"(0x1234ull) : "memory");
	printf("swpal x: old=%llx now=%llx\n", (unsigned long long)old, (unsigned long long)x);
	uint32_t o32;
	__asm__ volatile("swpalb %w2, %w0, [%1]" : "=r"(o32) : "r"(&b), "r"(0x42u) : "memory");
	printf("swpalb: old=%x now=%x\n", o32, b);

	uint64_t expect = 0x1234, desired = 0x5678;
	__asm__ volatile("casal %0, %2, [%1]" : "+r"(expect) : "r"(&x), "r"(desired) : "memory");
	printf("casal hit: old=%llx now=%llx\n", (unsigned long long)expect, (unsigned long long)x);
	expect = 1;
	__asm__ volatile("casl %0, %2, [%1]" : "+r"(expect) : "r"(&x), "r"(desired + 1) : "memory");
	printf("casl miss: old=%llx now=%llx\n", (unsigned long long)expect, (unsigned long long)x);
	uint32_t e32 = 0x42;
	__asm__ volatile("casalb %w0, %w2, [%1]" : "+r"(e32) : "r"(&b), "r"(0x99u) : "memory");
	printf("casalb: old=%x now=%x\n", e32, b);
	__asm__ volatile("stadd %w1, [%0]" : : "r"(&w), "r"(100u) : "memory");
	printf("stadd: now=%x\n", w);

	// And what the compiler generates for C11 atomics.
	_Atomic long counter = 40;
	long prev = atomic_fetch_add(&counter, 2);
	long cmp = 42;
	int ok = atomic_compare_exchange_strong(&counter, &cmp, 7);
	printf("c11: prev=%ld ok=%d now=%ld xchg=%ld\n", prev, ok, (long)counter, atomic_exchange(&counter, 9));
	return 0;
}
