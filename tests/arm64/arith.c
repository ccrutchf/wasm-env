// Differential fixture: integer arithmetic, logic, shifts, bitfields,
// flags and conditional selects. Prints results as hex; tests/m2.sh
// compares the output under qemu and translated.
#include <stdint.h>
#include <stdio.h>

// Inputs the compiler can't see through, so it emits real instructions.
static volatile uint64_t in64[] = {
	0, 1, 2, 3, 7, 0xff, 0x100, 0x7fffffff, 0x80000000, 0xffffffff,
	0x100000000, 0x123456789abcdef0, 0x7fffffffffffffff, 0x8000000000000000,
	0xfffffffffffffffe, 0xffffffffffffffff,
};
#define N (sizeof in64 / sizeof in64[0])

struct bits { unsigned lo : 5, mid : 11, hi : 16; };

static void out(const char *what, uint64_t v) { printf("%s %016llx\n", what, (unsigned long long)v); }

int main(void)
{
	for (unsigned i = 0; i < N; i++) {
		for (unsigned j = 0; j < N; j += 3) {
			uint64_t a = in64[i], b = in64[j];
			uint32_t a32 = (uint32_t)a, b32 = (uint32_t)b;
			int64_t sa = (int64_t)a, sb = (int64_t)b;
			printf("[%u %u]\n", i, j);
			out("add", a + b);
			out("sub", a - b);
			out("add32", a32 + b32);
			out("sub32", a32 - b32);
			out("mul", a * b);
			out("mul32", (uint64_t)(a32 * b32));
			out("umulh", (uint64_t)(((unsigned __int128)a * b) >> 64));
			out("smulh", (uint64_t)(((__int128)sa * sb) >> 64));
			out("umull", (uint64_t)a32 * b32);
			out("smull", (uint64_t)((int64_t)(int32_t)a32 * (int32_t)b32));
			out("smnegl", (uint64_t)-((int64_t)(int32_t)a32 * (int32_t)b32));
			out("umnegl", -((uint64_t)a32 * b32));
			out("madd", a * b + in64[(i + 1) % N]);
			out("msub", in64[(i + 1) % N] - a * b);
			if (b) out("udiv", a / b);
			if (b32) out("udiv32", a32 / b32);
			if (b && !(sa == INT64_MIN && sb == -1)) out("sdiv", (uint64_t)(sa / sb));
			if (b) out("urem", a % b);
			out("and", a & b);
			out("orr", a | b);
			out("eor", a ^ b);
			out("bic", a & ~b);
			out("orn", a | ~b);
			out("lsl", a << (b & 63));
			out("lsr", a >> (b & 63));
			out("asr", (uint64_t)(sa >> (b & 63)));
			out("ror", (a >> (b & 63)) | (a << ((64 - (b & 63)) & 63)));
			out("lsl32", (uint32_t)(a32 << (b32 & 31)));
			out("asr32", (uint32_t)((int32_t)a32 >> (b32 & 31)));
			out("lt", sa < sb);
			out("ltu", a < b);
			out("le", sa <= sb);
			out("gt32", (int32_t)a32 > (int32_t)b32);
			out("hi", a > b);
			out("eq", a == b);
			out("csel", sa < sb ? a : b);
			out("csinc", a == b ? a : b + 1);
			out("csneg", sa >= sb ? a : -b);
			out("ccmp", (a > 3 && b < 100) || a == b);
			out("carry", (a + b) < a);
			out("ovf", __builtin_add_overflow(sa, sb, &sa));
		}
		uint64_t a = in64[i];
		uint32_t a32 = (uint32_t)a;
		out("clz", a ? (uint64_t)__builtin_clzll(a) : 64);
		out("ctz", a ? (uint64_t)__builtin_ctzll(a) : 64); // rbit + clz on arm64
		out("clz32", a32 ? (uint64_t)__builtin_clz(a32) : 32);
		out("popcount", (uint64_t)__builtin_popcountll(a));
		out("bswap", __builtin_bswap64(a));
		out("bswap32", __builtin_bswap32(a32));
		out("bswap16", __builtin_bswap16((uint16_t)a));
		out("neg", -a);
		out("not", ~a);
		out("sxtb", (uint64_t)(int64_t)(int8_t)a);
		out("sxth", (uint64_t)(int64_t)(int16_t)a);
		out("sxtw", (uint64_t)(int64_t)(int32_t)a);
		out("uxtb", (uint8_t)a);
		out("ubfx", (a >> 13) & 0x7ff);
		out("sbfx", (uint64_t)(((int64_t)(a << 20)) >> 50));
		struct bits s = { .lo = (unsigned)a, .mid = (unsigned)(a >> 8), .hi = (unsigned)(a >> 30) };
		s.mid += 3;
		out("bitfield", (uint64_t)s.lo << 40 | (uint64_t)s.mid << 20 | s.hi);
		out("mov16", (a & ~0xffff0000ull) | 0xbeef0000);
	}
	return 0;
}
