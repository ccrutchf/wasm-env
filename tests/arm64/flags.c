// NZCV from adds/subs/adcs/sbcs/negs at 64 and 32 bits, over edge values and
// both carry-ins, read back with cset. Differential against qemu.
#include <stdint.h>
#include <stdio.h>

#define NZCV(out) \
	"cset %w[n], mi\n\tcset %w[z], eq\n\tcset %w[c], cs\n\tcset %w[v], vs\n\t"

static const uint64_t vals[] = { 0, 1, 2, 0x7fffffffffffffffull, 0x8000000000000000ull, 0x8000000000000001ull,
	                             0xffffffffffffffffull, 0xfffffffffffffffeull, 0x7fffffff, 0x80000000, 0xffffffff,
	                             0x123456789abcdef0ull };
#define N (sizeof vals / sizeof vals[0])

int main(void)
{
	unsigned long sum = 0;
	for (unsigned i = 0; i < N; i++)
		for (unsigned j = 0; j < N; j++)
			for (int cin = 0; cin < 2; cin++) {
				uint64_t a = vals[i], b = vals[j], r[5];
				uint32_t r32[4];
				int n[9], z[9], c[9], v[9];
				// Carry in: cmp x0, x0 sets C; cmn xzr... clears it via adds 0+0.
#define OP(k, insn, dst, reg)                                                                           \
	__asm__ volatile("cmp %w[ci], #1\n\t" insn "\n\t" NZCV()                                          \
	                 : [r] "=&r"(dst), [n] "=&r"(n[k]), [z] "=&r"(z[k]), [c] "=&r"(c[k]), [v] "=&r"(v[k]) \
	                 : [a] "r"(a), [b] "r"(b), [ci] "r"(cin)                                           \
	                 : "cc")
				OP(0, "adds %[r], %[a], %[b]", r[0], x);
				OP(1, "subs %[r], %[a], %[b]", r[1], x);
				OP(2, "adcs %[r], %[a], %[b]", r[2], x);
				OP(3, "sbcs %[r], %[a], %[b]", r[3], x);
				OP(4, "negs %[r], %[a]", r[4], x);
				OP(5, "adds %w[r], %w[a], %w[b]", r32[0], w);
				OP(6, "subs %w[r], %w[a], %w[b]", r32[1], w);
				OP(7, "adcs %w[r], %w[a], %w[b]", r32[2], w);
				OP(8, "sbcs %w[r], %w[a], %w[b]", r32[3], w);
				for (int k = 0; k < 9; k++)
					sum = sum * 31 + (n[k] << 3 | z[k] << 2 | c[k] << 1 | v[k]);
				for (int k = 0; k < 5; k++)
					sum = sum * 31 + r[k];
				for (int k = 0; k < 4; k++)
					sum = sum * 31 + r32[k];
				if (i == 3 && (j == 1 || j == 4))
					printf("a=%llx b=%llx cin=%d adds nzcv=%d%d%d%d adcs=%d%d%d%d sbcs=%d%d%d%d\n",
					       (unsigned long long)a, (unsigned long long)b, cin, n[0], z[0], c[0], v[0], n[2],
					       z[2], c[2], v[2], n[3], z[3], c[3], v[3]);
			}
	printf("checksum %lx\n", sum);
	return 0;
}
