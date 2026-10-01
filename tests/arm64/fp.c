// Floating-point rounding/conversion and NEON operations used by libc and
// libm: the frint* family, fcvtas, int<->fp through vector registers, and
// NEON bitwise select, widening, byte reversal, table lookup, vector fneg
// and mvni with a shifted-ones immediate. Instructions are forced with
// inline asm where the compiler might pick a different encoding.
#include <arm_neon.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const double inputs[] = { 0.0, -0.0, 0.5, 1.5, 2.5, -0.5, -1.5, -2.5, 2.4999999999999996,
	0.49999999999999994, 1e300, -3.7, 123456789.75, INFINITY, -INFINITY };
#define N (sizeof inputs / sizeof inputs[0])

// Hides a value from the optimizer, so NEON intrinsics on constants aren't
// folded away at compile time.
#define OPAQUE(v) __asm__("" : "+w"(v))

#define FRINT(name)                                                         \
	static double name(double x)                                        \
	{                                                                   \
		double r;                                                   \
		__asm__(#name " %d0, %d1" : "=w"(r) : "w"(x));              \
		return r;                                                   \
	}
FRINT(frintx)
FRINT(frinti)
FRINT(frintz)
FRINT(frintp)
FRINT(frintm)
FRINT(frinta)
FRINT(frintn)

static float frinta_s(float x)
{
	float r;
	__asm__("frinta %s0, %s1" : "=w"(r) : "w"(x));
	return r;
}

static long fcvtas(double x)
{
	long r;
	__asm__("fcvtas %0, %d1" : "=r"(r) : "w"(x));
	return r;
}

static uint64_t bits(double d)
{
	uint64_t u;
	memcpy(&u, &d, 8);
	return u;
}

static void print_v(const char *name, uint8x16_t v)
{
	uint8_t b[16];
	vst1q_u8(b, v);
	printf("%s:", name);
	for (int i = 0; i < 16; i++) printf(" %02x", b[i]);
	printf("\n");
}

int main(void)
{
	for (unsigned i = 0; i < N; i++) {
		double x = inputs[i];
		printf("%-24.17g x=%016llx z=%016llx p=%016llx m=%016llx a=%016llx n=%016llx i=%016llx\n", x,
		       (unsigned long long)bits(frintx(x)), (unsigned long long)bits(frintz(x)),
		       (unsigned long long)bits(frintp(x)), (unsigned long long)bits(frintm(x)),
		       (unsigned long long)bits(frinta(x)), (unsigned long long)bits(frintn(x)),
		       (unsigned long long)bits(frinti(x)));
		if (isfinite(x) && fabs(x) < 1e18) printf("  fcvtas=%ld\n", fcvtas(x));
	}
	printf("frinta_s(2.5f)=%a frinta_s(-0.5f)=%a\n", frinta_s(2.5f), frinta_s(-0.5f));

	// libm, which uses the same instructions.
	printf("libm: %g %g %g %g %g %ld %lld %g\n", floor(-3.7), ceil(-3.7), trunc(-3.7), round(-2.5), rint(2.5),
	       lround(-2.5), llround(7.5), nearbyint(3.5));
	printf("libmf: %g %g %g %g\n", (double)floorf(2.5f), (double)ceilf(2.5f), (double)roundf(-0.5f),
	       (double)truncf(-9.99f));

	// int <-> fp with the integer in a SIMD register.
	int64_t i64 = -1234567890123;
	double d;
	__asm__("fmov d31, %1\n\tscvtf %d0, d31" : "=w"(d) : "r"(i64) : "v31");
	printf("scvtf d,d: %.1f\n", d);
	uint64_t u64 = 18000000000000000000ull;
	__asm__("fmov d31, %1\n\tucvtf %d0, d31" : "=w"(d) : "r"(u64) : "v31");
	printf("ucvtf d,d: %.1f\n", d);
	int32_t i32 = -42;
	float f;
	__asm__("fmov s31, %w1\n\tscvtf %s0, s31" : "=w"(f) : "r"(i32) : "v31");
	printf("scvtf s,s: %.1f\n", (double)f);

	// NEON.
	uint8x16_t a = vcombine_u8(vcreate_u8(0x0706050403020100ull), vcreate_u8(0x0f0e0d0c0b0a0908ull));
	uint8x16_t b = vdupq_n_u8(0xaa);
	uint8x16_t mask = vcombine_u8(vcreate_u8(0xff00ff00f0f00f0full), vcreate_u8(0x00000000ffffffffull));
	OPAQUE(a);
	OPAQUE(b);
	OPAQUE(mask);
	uint8x16_t r;
	r = b;
	__asm__("bif %0.16b, %1.16b, %2.16b" : "+w"(r) : "w"(a), "w"(mask));
	print_v("bif", r);
	r = b;
	__asm__("bit %0.16b, %1.16b, %2.16b" : "+w"(r) : "w"(a), "w"(mask));
	print_v("bit", r);
	r = mask;
	__asm__("bsl %0.16b, %1.16b, %2.16b" : "+w"(r) : "w"(a), "w"(b));
	print_v("bsl", r);
	r = b;
	__asm__("bif %0.8b, %1.8b, %2.8b" : "+w"(r) : "w"(a), "w"(mask));
	print_v("bif.8b", r);

	print_v("rev32", vrev32q_u8(a));
	print_v("rev16", vrev16q_u8(a));
	print_v("rev64", vrev64q_u8(a));
	print_v("rev32.8h", vreinterpretq_u8_u16(vrev32q_u16(vreinterpretq_u16_u8(a))));

	uint8x16_t idx = vcombine_u8(vcreate_u8(0x0001020304050607ull), vcreate_u8(0xff10200f08090a0bull));
	OPAQUE(idx);
	print_v("tbl", vqtbl1q_u8(a, idx));
	uint8x16x2_t t2 = { { a, vaddq_u8(a, vdupq_n_u8(0x10)) } };
	print_v("tbl2", vqtbl2q_u8(t2, idx));

	int16x4_t s16 = vcreate_s16(0x8000ffff7fff0001ull);
	OPAQUE(s16);
	print_v("sxtl", vreinterpretq_u8_s32(vmovl_s16(s16)));
	uint32x4_t u32s = vreinterpretq_u32_u8(a);
	print_v("uxtl", vreinterpretq_u8_u64(vmovl_u32(vget_low_u32(u32s))));
	print_v("uxtl2", vreinterpretq_u8_u64(vmovl_high_u32(u32s)));
	print_v("sxtl2", vreinterpretq_u8_s16(vmovl_high_s8(vreinterpretq_s8_u8(vaddq_u8(a, vdupq_n_u8(0x78))))));

	float64x2_t fd = { 1.5, -2.25 };
	OPAQUE(fd);
	print_v("fneg.2d", vreinterpretq_u8_f64(vnegq_f64(fd)));
	float32x4_t fs = { 1.0f, -0.0f, 3.5f, -7.0f };
	OPAQUE(fs);
	print_v("fneg.4s", vreinterpretq_u8_f32(vnegq_f32(fs)));
	print_v("fabs.4s", vreinterpretq_u8_f32(vabsq_f32(fs)));

	uint32x2_t mv;
	__asm__("mvni %0.2s, #0xf, msl #8" : "=w"(mv));
	printf("mvni msl8: %08x %08x\n", vget_lane_u32(mv, 0), vget_lane_u32(mv, 1));
	__asm__("mvni %0.2s, #0x3c, msl #16" : "=w"(mv));
	printf("mvni msl16: %08x\n", vget_lane_u32(mv, 0));
	uint32x4_t mo;
	__asm__("movi %0.4s, #0x3c, msl #8" : "=w"(mo));
	printf("movi msl8: %08x\n", vgetq_lane_u32(mo, 3));

	int8x16_t n8 = vreinterpretq_s8_u8(a);
	print_v("neg.16b", vreinterpretq_u8_s8(vnegq_s8(n8)));
	print_v("rbit", vrbitq_u8(a));

	// A 64-bit whole-register move clears the destination's upper half.
	uint8x16_t mv8 = vdupq_n_u8(0x5a);
	OPAQUE(mv8);
	__asm__("mov %0.8b, %1.8b" : "+w"(mv8) : "w"(a));
	print_v("mov.8b", mv8);

	uint8x16_t p = vaddq_u8(a, vdupq_n_u8(0x7b));
	OPAQUE(p);

	// Widening and pairwise-long arithmetic.
	int64x2_t w64 = vreinterpretq_s64_u8(mask);
	int32x4_t n32 = vreinterpretq_s32_u8(p);
	print_v("ssubw", vreinterpretq_u8_s64(vsubw_s32(w64, vget_low_s32(n32))));
	print_v("ssubw2", vreinterpretq_u8_s64(vsubw_high_s32(w64, n32)));
	print_v("uaddw2", vreinterpretq_u8_u16(vaddw_high_u8(vreinterpretq_u16_u8(a), p)));
	print_v("saddl", vreinterpretq_u8_s32(vaddl_s16(vget_low_s16(vreinterpretq_s16_u8(p)), vget_low_s16(vreinterpretq_s16_u8(mask)))));
	print_v("uaddlp", vreinterpretq_u8_u16(vpaddlq_u8(p)));
	print_v("sadalp", vreinterpretq_u8_s32(vpadalq_s16(vreinterpretq_s32_u8(a), vreinterpretq_s16_u8(p))));

	// Scalar forms on SIMD registers.
	uint64_t sd;
	__asm__("add %d0, %d1, %d2" : "=w"(sd) : "w"(0x7fffffffffffffffull), "w"(3ull));
	printf("add d: %016llx\n", (unsigned long long)sd);
	double dv = -1234.75;
	int64_t cz;
	__asm__("fcvtzs %d0, %d1" : "=w"(cz) : "w"(dv));
	printf("fcvtzs d,d: %lld\n", (long long)cz);
	__asm__("fcvtzu %d0, %d1" : "=w"(cz) : "w"(dv));
	printf("fcvtzu d,d: %lld\n", (long long)cz);
	__asm__("fcvtzs %0, %d1, #4" : "=r"(cz) : "w"(dv));
	printf("fcvtzs fixed: %lld\n", (long long)cz);
	double fa;
	__asm__("fabd %d0, %d1, %d2" : "=w"(fa) : "w"(1.5), "w"(4.25));
	printf("fabd: %g\n", fa);
	print_v("fabd.4s", vreinterpretq_u8_f32(vabdq_f32(fs, vdupq_n_f32(2.0f))));
	float lane3;
	__asm__("mov %s0, %1.s[3]" : "=w"(lane3) : "w"(fs));
	printf("mov s, v.s[3]: %g\n", (double)lane3);
	// Scalar by element: the named lane, not lane 0.
	float fe;
	__asm__("fmul %s0, %s1, %2.s[2]" : "=w"(fe) : "w"(2.0f), "w"(fs));
	float64x2_t dv2 = { 3.0, 5.0 };
	double de;
	__asm__("fmul %d0, %d1, %2.d[1]" : "=w"(de) : "w"(1.5), "w"(dv2));
	printf("fmul by element: %g %g\n", (double)fe, de);
	print_v("fcvtzs.4s", vreinterpretq_u8_s32(vcvtq_s32_f32(fs)));

	// Interleaved and multi-register loads and stores.
	uint8_t buf[64], out[64];
	for (int i = 0; i < 64; i++) buf[i] = (uint8_t)(i * 7 + 3);
	OPAQUE(a);
	uint8x16x4_t q4 = vld4q_u8(buf);
	print_v("ld4.0", q4.val[0]);
	print_v("ld4.3", q4.val[3]);
	uint16x8x3_t q3 = vld3q_u16((const uint16_t *)buf);
	print_v("ld3.1", vreinterpretq_u8_u16(q3.val[1]));
	uint32x4x2_t q2 = vld2q_u32((const uint32_t *)buf);
	print_v("ld2.1", vreinterpretq_u8_u32(q2.val[1]));
	vst4q_u8(out, q4);
	printf("st4 roundtrip: %d\n", __builtin_memcmp(out, buf, 64) == 0);
	uint8x16x2_t two = vld1q_u8_x2(buf + 8);
	vst1q_u8_x2(out, two);
	printf("ld1/st1 x2: %d %02x\n", __builtin_memcmp(out, buf + 8, 32) == 0, vgetq_lane_u8(two.val[1], 15));

	// Pairwise add/min/max across the concatenation of two vectors.
	print_v("umaxp.4s", vreinterpretq_u8_u32(vpmaxq_u32(vreinterpretq_u32_u8(a), vreinterpretq_u32_u8(p))));
	print_v("uminp.8h", vreinterpretq_u8_u16(vpminq_u16(vreinterpretq_u16_u8(p), vreinterpretq_u16_u8(a))));
	print_v("smaxp.16b", vreinterpretq_u8_s8(vpmaxq_s8(vreinterpretq_s8_u8(p), vreinterpretq_s8_u8(mask))));
	print_v("sminp.4s", vreinterpretq_u8_s32(vpminq_s32(vreinterpretq_s32_u8(mask), vreinterpretq_s32_u8(p))));
	print_v("addp.2d", vreinterpretq_u8_u64(vpaddq_u64(vreinterpretq_u64_u8(p), vreinterpretq_u64_u8(mask))));
	return 0;
}
