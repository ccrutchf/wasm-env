// FPCR: rounding modes, flush to zero (FZ, FZ16) and default NaN (DN), for
// the instructions they change, in double, single and half precision, on
// edge cases (denormals, ties, overflow, NaNs with payloads) and random
// operands. Prints a checksum of each instruction's results (and FPSR's
// flags) under each setting; build with -DVERBOSE for every result.
// Also: fesetround, and FPCR carried into a new thread. (Under the default
// FPCR, NaNs propagate as WebAssembly's do: see run.)
#include <fenv.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>

static uint64_t get_fpcr(void)
{
	uint64_t v;
	__asm__ volatile("mrs %0, fpcr" : "=r"(v));
	return v;
}

static void set_fpcr(uint64_t v)
{
	__asm__ volatile("msr fpcr, %0" ::"r"(v));
}

static uint64_t get_fpsr(void)
{
	uint64_t v;
	__asm__ volatile("mrs %0, fpsr" : "=r"(v));
	return v;
}

static void set_fpsr(uint64_t v)
{
	__asm__ volatile("msr fpsr, %0" ::"r"(v));
}

// Scalar operations on raw bits: d, s or h registers.
#define OP1(name, insn, r)                                                                                             \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		(void)b, (void)c;                                                                                      \
		__asm__ volatile("fmov d0, %1\n" insn " " r "0, " r "0\nfmov %0, d0" : "=r"(x) : "r"(a) : "v0");    \
		return x;                                                                                              \
	}
#define OP2(name, insn, r)                                                                                             \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		(void)c;                                                                                               \
		__asm__ volatile("fmov d0, %1\nfmov d1, %2\n" insn " " r "0, " r "0, " r "1\nfmov %0, d0"           \
		                 : "=r"(x)                                                                             \
		                 : "r"(a), "r"(b)                                                                      \
		                 : "v0", "v1");                                                                        \
		return x;                                                                                              \
	}
#define OP3(name, insn, r)                                                                                             \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		__asm__ volatile("fmov d0, %1\nfmov d1, %2\nfmov d2, %3\n" insn " " r "0, " r "0, " r "1, " r       \
		                 "2\nfmov %0, d0"                                                                      \
		                 : "=r"(x)                                                                             \
		                 : "r"(a), "r"(b), "r"(c)                                                              \
		                 : "v0", "v1", "v2");                                                                  \
		return x;                                                                                              \
	}
// Comparisons: NZCV.
#define CMP(name, r)                                                                                                   \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		(void)c;                                                                                               \
		__asm__ volatile("fmov d0, %1\nfmov d1, %2\nfcmp " r "0, " r "1\nmrs %0, nzcv"                       \
		                 : "=r"(x)                                                                             \
		                 : "r"(a), "r"(b)                                                                      \
		                 : "v0", "v1", "cc");                                                                  \
		return x;                                                                                              \
	}
// Conversions: from register class `from` to `to`.
#define CVT(name, insn, to, from)                                                                                      \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		(void)b, (void)c;                                                                                      \
		__asm__ volatile("fmov d0, %1\n" insn " " to "0, " from "0\nfmov %0, d0" : "=r"(x) : "r"(a) : "v0");\
		return x;                                                                                              \
	}
#define FROM_INT(name, insn, to, from, extra)                                                                          \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		(void)b, (void)c;                                                                                      \
		__asm__ volatile(insn " " to "0, %" from "1" extra "\nfmov %0, d0" : "=r"(x) : "r"(a) : "v0");      \
		return x;                                                                                              \
	}
#define TO_INT(name, insn, r)                                                                                          \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		(void)b, (void)c;                                                                                      \
		__asm__ volatile("fmov d0, %1\n" insn " %0, " r "0" : "=r"(x) : "r"(a) : "v0");                      \
		return x;                                                                                              \
	}
// Vectors: two lanes of a and b (a, b in lane 0; b, c in lane 1), the
// result's low 64 bits.
#define VOP2(name, insn, arr, ins)                                                                                     \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		__asm__ volatile("fmov d0, %1\nfmov d1, %2\nins v0." ins "[1], %2\nins v1." ins "[1], %3\n" insn     \
		                 " v0." arr ", v0." arr ", v1." arr "\nfmov %0, d0"                                    \
		                 : "=r"(x)                                                                             \
		                 : "r"(a), "r"(b), "r"(c)                                                              \
		                 : "v0", "v1");                                                                        \
		return x;                                                                                              \
	}
#define VOP2HI(name, insn, arr, ins)                                                                                   \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		__asm__ volatile("fmov d0, %1\nfmov d1, %2\nins v0." ins "[1], %2\nins v1." ins "[1], %3\n" insn     \
		                 " v0." arr ", v0." arr ", v1." arr "\nmov %0, v0.d[1]"                                \
		                 : "=r"(x)                                                                             \
		                 : "r"(a), "r"(b), "r"(c)                                                              \
		                 : "v0", "v1");                                                                        \
		return x;                                                                                              \
	}
#define VOP1(name, insn, arr, ins)                                                                                     \
	static uint64_t name(uint64_t a, uint64_t b, uint64_t c)                                                       \
	{                                                                                                              \
		uint64_t x;                                                                                            \
		(void)c;                                                                                               \
		__asm__ volatile("fmov d0, %1\nins v0." ins "[1], %2\n" insn " v0." arr ", v0." arr "\nmov %0, v0.d[1]" \
		                 : "=r"(x)                                                                             \
		                 : "r"(a), "r"(b)                                                                      \
		                 : "v0");                                                                              \
		return x;                                                                                              \
	}

OP2(add_d, "fadd", "d") OP2(add_s, "fadd", "s") OP2(add_h, "fadd", "h")
OP2(sub_d, "fsub", "d") OP2(sub_s, "fsub", "s") OP2(sub_h, "fsub", "h")
OP2(mul_d, "fmul", "d") OP2(mul_s, "fmul", "s") OP2(mul_h, "fmul", "h")
OP2(div_d, "fdiv", "d") OP2(div_s, "fdiv", "s") OP2(div_h, "fdiv", "h")
OP2(nmul_d, "fnmul", "d") OP2(nmul_s, "fnmul", "s")
OP2(abd_d, "fabd", "d") OP2(abd_s, "fabd", "s")
OP2(min_d, "fmin", "d") OP2(max_s, "fmax", "s") OP2(minnm_d, "fminnm", "d") OP2(maxnm_s, "fmaxnm", "s")
OP3(madd_d, "fmadd", "d") OP3(madd_s, "fmadd", "s") OP3(madd_h, "fmadd", "h")
OP3(msub_d, "fmsub", "d") OP3(nmadd_s, "fnmadd", "s") OP3(nmsub_d, "fnmsub", "d")
OP1(sqrt_d, "fsqrt", "d") OP1(sqrt_s, "fsqrt", "s") OP1(sqrt_h, "fsqrt", "h")
OP1(rinti_d, "frinti", "d") OP1(rintx_s, "frintx", "s") OP1(rintp_d, "frintp", "d") OP1(rinta_s, "frinta", "s")
OP1(rintn_h, "frintn", "h")
CMP(cmp_d, "d") CMP(cmp_s, "s")
CVT(cvt_sd, "fcvt", "s", "d") CVT(cvt_ds, "fcvt", "d", "s") CVT(cvt_hd, "fcvt", "h", "d") CVT(cvt_hs, "fcvt", "h", "s")
CVT(cvt_sh, "fcvt", "s", "h") CVT(cvt_dh, "fcvt", "d", "h")
FROM_INT(scvtf_dx, "scvtf", "d", "x", "") FROM_INT(ucvtf_sx, "ucvtf", "s", "x", "") FROM_INT(scvtf_sw, "scvtf", "s", "w", "")
FROM_INT(ucvtf_dw, "ucvtf", "d", "w", "") FROM_INT(scvtf_dx12, "scvtf", "d", "x", ", #12")
FROM_INT(scvtf_hx, "scvtf", "h", "x", "")
TO_INT(cvtps_d, "fcvtps", "d") TO_INT(cvtzs_s, "fcvtzs", "s")
VOP2(vadd_2d, "fadd", "2d", "d") VOP2HI(vadd_2dhi, "fadd", "2d", "d") VOP2(vmul_4s, "fmul", "4s", "d")
VOP2(vdiv_4s, "fdiv", "4s", "d") VOP2(vsub_2s, "fsub", "2s", "d") VOP2(vmla_2d, "fmla", "2d", "d")
VOP2(vmin_4s, "fmin", "4s", "d") VOP2(vmaxnm_2d, "fmaxnm", "2d", "d") VOP2(vaddp_4s, "faddp", "4s", "d")
VOP2(vcmge_2d, "fcmge", "2d", "d")
VOP1(vsqrt_2d, "fsqrt", "2d", "d") VOP1(vrinti_2d, "frinti", "2d", "d") VOP1(vscvtf_2d, "scvtf", "2d", "d")

static uint64_t vcvtn(uint64_t a, uint64_t b, uint64_t c)
{
	uint64_t x;
	(void)c;
	__asm__ volatile("fmov d0, %1\nins v0.d[1], %2\nfcvtn v1.2s, v0.2d\nfmov %0, d1" : "=r"(x) : "r"(a), "r"(b) : "v0", "v1");
	return x;
}

static uint64_t vcvtl(uint64_t a, uint64_t b, uint64_t c)
{
	uint64_t x;
	(void)b, (void)c;
	__asm__ volatile("fmov d0, %1\nfcvtl v1.2d, v0.2s\nmov %0, v1.d[1]" : "=r"(x) : "r"(a) : "v0", "v1");
	return x;
}

// Format of an operation's operands: 64, 32 or 16 bits (F_INT: integers).
enum { F64, F32, F16, F_INT };
struct op {
	const char *name;
	uint64_t (*fn)(uint64_t, uint64_t, uint64_t);
	int fmt;
	// Packs two operands into one (64-bit lanes: no; 32: two per 64).
	int pack;
};
static const struct op ops[] = {
	{"fadd d", add_d, F64}, {"fadd s", add_s, F32}, {"fadd h", add_h, F16},
	{"fsub d", sub_d, F64}, {"fsub s", sub_s, F32}, {"fsub h", sub_h, F16},
	{"fmul d", mul_d, F64}, {"fmul s", mul_s, F32}, {"fmul h", mul_h, F16},
	{"fdiv d", div_d, F64}, {"fdiv s", div_s, F32}, {"fdiv h", div_h, F16},
	{"fnmul d", nmul_d, F64}, {"fnmul s", nmul_s, F32},
	{"fabd d", abd_d, F64}, {"fabd s", abd_s, F32},
	{"fmin d", min_d, F64}, {"fmax s", max_s, F32}, {"fminnm d", minnm_d, F64}, {"fmaxnm s", maxnm_s, F32},
	{"fmadd d", madd_d, F64}, {"fmadd s", madd_s, F32}, {"fmadd h", madd_h, F16},
	{"fmsub d", msub_d, F64}, {"fnmadd s", nmadd_s, F32}, {"fnmsub d", nmsub_d, F64},
	{"fsqrt d", sqrt_d, F64}, {"fsqrt s", sqrt_s, F32}, {"fsqrt h", sqrt_h, F16},
	{"frinti d", rinti_d, F64}, {"frintx s", rintx_s, F32}, {"frintp d", rintp_d, F64}, {"frinta s", rinta_s, F32},
	{"frintn h", rintn_h, F16},
	{"fcmp d", cmp_d, F64}, {"fcmp s", cmp_s, F32},
	{"fcvt s<-d", cvt_sd, F64}, {"fcvt d<-s", cvt_ds, F32}, {"fcvt h<-d", cvt_hd, F64}, {"fcvt h<-s", cvt_hs, F32},
	{"fcvt s<-h", cvt_sh, F16}, {"fcvt d<-h", cvt_dh, F16},
	{"scvtf d<-x", scvtf_dx, F_INT}, {"ucvtf s<-x", ucvtf_sx, F_INT}, {"scvtf s<-w", scvtf_sw, F_INT},
	{"ucvtf d<-w", ucvtf_dw, F_INT}, {"scvtf d<-x,12", scvtf_dx12, F_INT}, {"scvtf h<-x", scvtf_hx, F_INT},
	{"fcvtps x<-d", cvtps_d, F64}, {"fcvtzs x<-s", cvtzs_s, F32},
	{"fadd 2d", vadd_2d, F64}, {"fadd 2d hi", vadd_2dhi, F64}, {"fmul 4s", vmul_4s, F32, 1},
	{"fdiv 4s", vdiv_4s, F32, 1}, {"fsub 2s", vsub_2s, F32, 1}, {"fmla 2d", vmla_2d, F64},
	{"fmin 4s", vmin_4s, F32, 1}, {"fmaxnm 2d", vmaxnm_2d, F64}, {"faddp 4s", vaddp_4s, F32, 1},
	{"fcmge 2d", vcmge_2d, F64},
	{"fsqrt 2d", vsqrt_2d, F64}, {"frinti 2d", vrinti_2d, F64}, {"scvtf 2d", vscvtf_2d, F_INT},
	{"fcvtn 2s<-2d", vcvtn, F64}, {"fcvtl 2d<-2s", vcvtl, F32, 1},
};

// Operands of each format: edge cases with either sign, then random ones.
#define NVALS 64
static uint64_t vals[4][NVALS];

static uint64_t rng = 0x9E3779B97F4A7C15;
static uint64_t next(void)
{
	rng ^= rng << 13;
	rng ^= rng >> 7;
	rng ^= rng << 17;
	return rng;
}

static void make_values(void)
{
	static const int width[3] = {64, 32, 16}, prec[3] = {53, 24, 11};
	for (int f = 0; f < 3; f++) {
		int n = width[f], p = prec[f], ew = n - p;
		uint64_t one = ((1ull << (ew - 1)) - 1) << (p - 1), ulp = 1, emax = (1ull << ew) - 1;
		uint64_t inf = emax << (p - 1), quiet = 1ull << (p - 2), min = 1ull << (p - 1);
		uint64_t edge[] = {
			0, 1, min - 1, min, min + 1, min / 2, min + min / 2, one, one + ulp, one - ulp,
			one + quiet,                     // 1.5
			one + (2ull << (p - 1)) + quiet, // 6, tie-ish for rint (×2^2)
			one - (1ull << (p - 1)),         // 0.5
			one - (1ull << (p - 1)) + quiet, // 0.75
			inf - 1, inf - 2, inf, inf | quiet | 5, inf | 3, (one + (1ull << (p - 1))) | 0x15,
			(one + 3 * (1ull << (p - 1)) + quiet), // 2.5 * 2^... (rint ties)
		};
		int k = 0;
		for (unsigned i = 0; i < sizeof edge / sizeof *edge; i++) {
			vals[f][k++] = edge[i];
			vals[f][k++] = edge[i] | 1ull << (n - 1);
		}
		uint64_t mask = n == 64 ? ~0ull : (1ull << n) - 1;
		while (k < NVALS) {
			uint64_t r = next() & mask;
			// Half of them near 1 (sums and products in range), some tiny.
			if (k % 3 == 0)
				r = (r & ~(emax << (p - 1))) | (one + ((next() % 9) << (p - 1)) - (4ull << (p - 1)));
			else if (k % 5 == 0)
				r &= ~(emax << (p - 1)) | (3ull << (p - 1));
			vals[f][k++] = r;
		}
	}
	// Integers: small, large, edges.
	uint64_t ints[] = {0, 1, 3, 7, (1ull << 53) + 1, (1ull << 24) + 1, ~0ull, 1ull << 63, (1ull << 63) - 1,
	                   0xFFFFFFFF, 0x80000000, 0x7FFFFFFF, 0x80000001, 4097, 65521, (1ull << 54) + 3};
	int k = 0;
	for (unsigned i = 0; i < sizeof ints / sizeof *ints; i++)
		vals[F_INT][k++] = ints[i];
	while (k < NVALS)
		vals[F_INT][k++] = next() >> (next() % 64);
}

static uint64_t operand(int fmt, int pack, int i)
{
	uint64_t v = vals[fmt][i % NVALS];
	if (pack)
		v = (v & 0xFFFFFFFF) | vals[fmt][(i * 7 + 3) % NVALS] << 32;
	return v;
}

static const struct {
	const char *name;
	uint64_t fpcr;
} settings[] = {
	{"RN", 0},
	{"RP", 1 << 22},
	{"RM", 2 << 22},
	{"RZ", 3 << 22},
	{"FZ", 1 << 24},
	{"DN", 1 << 25},
	{"FZ16", 1 << 19},
	{"RP+FZ+DN", 1 << 22 | 1 << 24 | 1 << 25},
	{"RM+FZ16+DN", 2 << 22 | 1 << 19 | 1 << 25},
};

// Whether `v` (format fmt; two singles if packed) holds a NaN.
static int has_nan(uint64_t v, int fmt, int pack)
{
	switch (fmt) {
	case F64:
		return (v & 0x7FFFFFFFFFFFFFFF) > 0x7FF0000000000000;
	case F32:
		return (v & 0x7FFFFFFF) > 0x7F800000 || (pack && (v >> 32 & 0x7FFFFFFF) > 0x7F800000);
	case F16:
		return (v & 0x7FFF) > 0x7C00;
	}
	return 0;
}

// An operation's result format, from its name ("fcvt s<-d", "fmul 4s"):
// sets *pack for two singles.
static int out_format(const char *name, int *pack)
{
	const char *d = name;
	while (*d && *d != ' ')
		d++;
	d++;
	*pack = d[0] == '4' || (d[0] == '2' && d[1] == 's');
	if (name[1] == 'c' && name[2] == 'm') // fcmp, fcmge: flags, masks
		return F_INT;
	switch (d[0] == '2' || d[0] == '4' ? d[1] : d[0]) {
	case 'd':
		return F64;
	case 's':
		return F32;
	case 'h':
		return F16;
	}
	return F_INT;
}

static void run(const char *name, uint64_t fpcr)
{
	for (unsigned o = 0; o < sizeof ops / sizeof *ops; o++) {
		const struct op *op = &ops[o];
		int out_pack, out = out_format(op->name, &out_pack);
		uint64_t sum = 0xcbf29ce484222325, flags = 0;
		for (int i = 0; i < NVALS; i++)
			for (int j = 0; j < NVALS; j++) {
				uint64_t a = operand(op->fmt, op->pack, i), b = operand(op->fmt, op->pack, j);
				uint64_t c = operand(op->fmt, op->pack, i * 5 + j * 3 + 1);
				// The default FPCR: translated code propagates NaNs as
				// WebAssembly does (see Block::fpcr in the translator),
				// so only whether a result is NaN counts, from operands
				// that aren't.
				int plain = !fpcr;
				if (plain && (has_nan(a, op->fmt, op->pack) || has_nan(b, op->fmt, op->pack) || has_nan(c, op->fmt, op->pack)))
					continue;
				set_fpsr(0);
				set_fpcr(fpcr);
				uint64_t r = op->fn(a, b, c);
				uint64_t s = get_fpsr() & 7; // IOC, DZC, OFC: what's modelled
				set_fpcr(0);
				if (plain && has_nan(r, out, out_pack))
					r = 1;
				flags |= s << 8 * (unsigned)(i % 3);
				sum = (sum ^ r ^ s << 60) * 0x100000001b3;
#ifdef VERBOSE
				printf("%s %s %016llx %016llx %016llx -> %016llx %llx\n", name, op->name, (unsigned long long)a,
				       (unsigned long long)b, (unsigned long long)c, (unsigned long long)r, (unsigned long long)s);
#endif
			}
		printf("%-12s %-16s %016llx %06llx\n", name, op->name, (unsigned long long)sum, (unsigned long long)flags);
	}
}

static void *in_thread(void *arg)
{
	(void)arg;
	printf("thread fpcr %llx\n", (unsigned long long)get_fpcr());
	volatile double x = 1.0, y = 3.0;
	double q = x / y;
	printf("thread 1/3 %a\n", q);
	return 0;
}

int main(void)
{
	make_values();
	for (unsigned s = 0; s < sizeof settings / sizeof *settings; s++)
		run(settings[s].name, settings[s].fpcr);

	// FPCR reads back what was written; the C library's rounding modes.
	set_fpcr(3 << 22 | 1 << 24);
	printf("fpcr %llx\n", (unsigned long long)get_fpcr());
	set_fpcr(0);
	static const int modes[] = {FE_TONEAREST, FE_UPWARD, FE_DOWNWARD, FE_TOWARDZERO};
	for (int m = 0; m < 4; m++) {
		fesetround(modes[m]);
		volatile double x = 1.0, y = 3.0, h = 2.5;
		volatile float f = 1.0f;
		printf("mode %d: 1/3 %a, -1/3 %a, 1/3f %a, rint(2.5) %a, (float)(1/3) %a\n", fegetround() == modes[m],
		       x / y, -x / y, f / 3.0f, __builtin_rint(h), (double)(float)(x / y));
	}
	fesetround(FE_TONEAREST);

	// A new thread starts with its creator's FPCR.
	set_fpcr(1 << 22);
	pthread_t t;
	pthread_create(&t, 0, in_thread, 0);
	pthread_join(t, 0);
	set_fpcr(0);
	return 0;
}
