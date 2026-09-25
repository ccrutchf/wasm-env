// NEON floating point and integer operations BLAS kernels and numpy use:
// fmla/fmls (vector, by element, scalar by element), pairwise and
// across-vector FP reductions, FP compares (register and zero), integer
// mla/mls, shift-right-accumulate, saturating narrows, shll, and
// half-precision conversions. Values are chosen so a fused and an unfused
// multiply-add agree.
#include <arm_neon.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/auxv.h>

#define OPAQUE(v) __asm__("" : "+w"(v))

static void print_v(const char *name, uint8x16_t v)
{
	uint8_t b[16];
	vst1q_u8(b, v);
	printf("%s:", name);
	for (int i = 0; i < 16; i++) printf(" %02x", b[i]);
	printf("\n");
}
#define P(name, v) print_v(name, (uint8x16_t)(v))

int main(void)
{
	float64x2_t a = { 1.5, -2.25 }, b = { 4.0, 0.5 }, c = { 10.0, 20.0 };
	float32x4_t fa = { 1.5f, -2.25f, 3.0f, 0.125f }, fb = { 4.0f, 0.5f, -8.0f, 16.0f }, fc = { 10.0f, 20.0f, 30.0f, 40.0f };
	OPAQUE(a); OPAQUE(b); OPAQUE(c); OPAQUE(fa); OPAQUE(fb); OPAQUE(fc);
	P("fmla.2d", vfmaq_f64(c, a, b));
	P("fmls.2d", vfmsq_f64(c, a, b));
	P("fmla.4s", vfmaq_f32(fc, fa, fb));
	P("fmla.2s", vcombine_f32(vfma_f32(vget_low_f32(fc), vget_low_f32(fa), vget_low_f32(fb)), vdup_n_f32(0)));
	P("fmla.4s lane", vfmaq_laneq_f32(fc, fa, fb, 3));
	P("fmls.2d lane", vfmsq_laneq_f64(c, a, b, 1));
	double d = 7.0, e = 3.0;
	__asm__("fmla %d0, %d1, %2.d[1]" : "+w"(d) : "w"(e), "w"(b));
	printf("fmla d lane: %g\n", d);
	float s = 1.0f;
	__asm__("fmls %s0, %s1, %2.s[2]" : "+w"(s) : "w"(2.0f), "w"(fb));
	printf("fmls s lane: %g\n", (double)s);

	printf("faddp d: %g  faddp s: %g\n", vpaddd_f64(a), (double)vpadds_f32(vget_low_f32(fa)));
	P("faddp.4s", vpaddq_f32(fa, fb));
	P("fmaxp.4s", vpmaxq_f32(fa, fb));
	P("fminp.2d", vpminq_f64(a, b));
	printf("fmaxp d: %g  fmaxnmp d: %g\n", vpmaxqd_f64(a), vpmaxnmqd_f64(b));
	printf("fmaxv: %g fminv: %g fmaxnmv: %g fminnmv: %g\n", (double)vmaxvq_f32(fa), (double)vminvq_f32(fb),
	       (double)vmaxnmvq_f32(fc), (double)vminnmvq_f32(fa));

	float32x4_t z = { 0.0f, -0.0f, 1.0f, -3.0f };
	OPAQUE(z);
	P("fcmeq", vceqq_f32(fa, z));
	P("fcmeq #0", vceqzq_f32(z));
	P("fcmgt", vcgtq_f32(fa, fb));
	P("fcmge", vcgeq_f64(a, b));
	P("fcmgt #0", vcgtzq_f32(z));
	P("fcmge #0", vcgezq_f32(z));
	P("fcmle #0", vclezq_f32(z));
	P("fcmlt #0", vcltzq_f32(z));
	P("facgt", vcagtq_f32(fa, fb));
	P("facge", vcageq_f64(a, b));
	printf("fcmgt d: %llx fcmeq d #0: %llx\n", (unsigned long long)vcgtd_f64(2.0, 1.0), (unsigned long long)vceqzd_f64(0.0));

	int32x4_t ia = { 3, -4, 100000, 7 }, ib = { 5, 6, -300, 2 }, ic = { 1, 1, 1, 1 };
	OPAQUE(ia); OPAQUE(ib); OPAQUE(ic);
	P("mla.4s", vmlaq_s32(ic, ia, ib));
	P("mls.4s", vmlsq_s32(ic, ia, ib));
	P("mla.4s lane", vmlaq_laneq_s32(ic, ia, ib, 2));
	P("mla.8h", vmlaq_s16(vreinterpretq_s16_s32(ic), vreinterpretq_s16_s32(ia), vreinterpretq_s16_s32(ib)));
	uint64x2_t acc = { 100, 200 }, big = { 0xffffffff00000000ull, 0x123456789ull };
	OPAQUE(acc); OPAQUE(big);
	P("usra #32", vsraq_n_u64(acc, big, 32));
	P("ssra #4", vsraq_n_s32(ia, ib, 4));
	int64x2_t wide = { 0x100000000ll, -5 };
	OPAQUE(wide);
	P("sqxtn", vcombine_s32(vqmovn_s64(wide), vdup_n_s32(0)));
	P("uqxtn", vcombine_u32(vqmovn_u64(big), vdup_n_u32(0)));
	P("sqxtun", vcombine_u16(vqmovun_s32(ia), vdup_n_u16(0)));
	P("sqxtn2", vqmovn_high_s64(vdup_n_s32(7), wide));
	int32_t n32;
	__asm__("sqxtn %s0, %d1" : "=w"(n32) : "w"(-0x100000000ll));
	printf("sqxtn s,d: %d\n", n32);
	uint8x8_t bytes = { 1, 2, 3, 250, 5, 6, 7, 255 };
	OPAQUE(bytes);
	P("shll", vshll_n_u8(bytes, 8));
	P("shll2", vshll_high_n_u8(vcombine_u8(bytes, vrev64_u8(bytes)), 8));

	float16x4_t h = vcvt_f16_f32(fa);
	P("fcvtn .4h", vcombine_f16(h, vdup_n_f16(0)));
	P("fcvtl .4s", vcvt_f32_f16(h));
	__fp16 one_h = 1.5;
	float fh;
	__asm__("fcvt %s0, %h1" : "=w"(fh) : "w"(one_h));
	printf("fcvt s,h: %g\n", (double)fh);
	__fp16 back;
	__asm__("fcvt %h0, %s1" : "=w"(back) : "w"(65504.0f * 2));
	printf("fcvt h,s overflow: %g\n", (double)(float)back);

	// Saturating add/subtract, and rounding conversions of whole vectors.
	uint8x16_t u8a = vdupq_n_u8(200), u8b = vcombine_u8(vcreate_u8(0x0102030405060708ull), vcreate_u8(0x9090909090909090ull));
	int16x8_t s16a = { 32000, -32000, 5, -5, 32767, -32768, 100, 0 }, s16b = { 1000, -1000, 7, 7, 1, -1, -200, 0 };
	OPAQUE(u8a); OPAQUE(u8b); OPAQUE(s16a); OPAQUE(s16b);
	P("uqadd", vqaddq_u8(u8a, u8b));
	P("uqsub", vqsubq_u8(u8b, u8a));
	P("sqadd", vqaddq_s16(s16a, s16b));
	P("sqsub", vqsubq_s16(s16a, s16b));
	float32x4_t halves = { 2.5f, -2.5f, 3.5f, 1e10f };
	OPAQUE(halves);
	P("fcvtns.4s", vcvtnq_s32_f32(halves));
	P("fcvtas.4s", vcvtaq_s32_f32(halves));
	P("fcvtms.4s", vcvtmq_s32_f32(halves));
	P("fcvtpu.4s", vcvtpq_u32_f32(halves));

	// Single-lane loads and stores (ld1/st1 {v.T}[k], and ld2-4/st2-4 lanes).
	double dmem[4] = { 1.25, 2.5, 3.75, 5.0 };
	float64x2_t dl = vdupq_n_f64(-1.0);
	OPAQUE(dl);
	dl = vld1q_lane_f64(&dmem[2], dl, 1);
	P("ld1 lane d1", dl);
	vst1q_lane_f64(&dmem[0], vcombine_f64(vdup_n_f64(7.0), vdup_n_f64(9.0)), 1);
	printf("st1 lane d1: %g %g %g %g\n", dmem[0], dmem[1], dmem[2], dmem[3]);
	uint32_t wmem[8] = { 1, 2, 3, 4, 5, 6, 7, 8 };
	uint32x4_t wl = vdupq_n_u32(0xaaaaaaaa);
	OPAQUE(wl);
	P("ld1 lane s2", vld1q_lane_u32(&wmem[5], wl, 2));
	vst1q_lane_u32(&wmem[7], vcombine_u32(vcreate_u32(0x1111111122222222ull), vcreate_u32(0x3333333344444444ull)), 3);
	printf("st1 lane s3: %x\n", wmem[7]);
	uint32x4x2_t pair = { { vdupq_n_u32(0x10), vdupq_n_u32(0x20) } };
	OPAQUE(pair.val[0]);
	pair = vld2q_lane_u32(&wmem[0], pair, 1);
	P("ld2 lane 0", pair.val[0]);
	P("ld2 lane 1", pair.val[1]);
	vst2q_lane_u32(&wmem[4], pair, 1);
	printf("st2 lane: %x %x\n", wmem[4], wmem[5]);
	{
		float64x2_t v = vdupq_n_f64(-1.0);
		double *p = &dmem[1];
		__asm__ volatile("ld1 {%0.d}[1], [%1], #8" : "+w"(v), "+r"(p) : : "memory");
		P("asm ld1 {d}[1] post", v);
		printf("  base advanced by %ld\n", (long)(p - dmem));
		uint32x4_t s = vcombine_u32(vcreate_u32(0x1111111122222222ull), vcreate_u32(0x3333333344444444ull));
		OPAQUE(s);
		__asm__ volatile("st1 {%1.s}[3], [%0]" : : "r"(&wmem[2]), "w"(s) : "memory");
		printf("asm st1 {s}[3]: %x\n", wmem[2]);
		uint8x16_t bb = vdupq_n_u8(0x55);
		OPAQUE(bb);
		__asm__ volatile("ld1 {%0.b}[5], [%1]" : "+w"(bb) : "r"(&wmem[0]) : "memory");
		P("asm ld1 {b}[5]", bb);
		uint16x8x3_t t3 = { { vdupq_n_u16(1), vdupq_n_u16(2), vdupq_n_u16(3) } };
		uint16_t hmem[3] = { 0xa1, 0xa2, 0xa3 };
		OPAQUE(t3.val[0]); OPAQUE(t3.val[1]); OPAQUE(t3.val[2]);
		t3 = vld3q_lane_u16(hmem, t3, 6);
		P("asm ld3 h[6] 0", t3.val[0]);
		P("asm ld3 h[6] 2", t3.val[2]);
	}
	{
		float64x2_t x = { 2.5, __builtin_nan("") }, repl = { 99.0, 77.0 };
		uint64x2_t m, cnt;
		float64x2_t out;
		uint64_t n;
		OPAQUE(x); OPAQUE(repl);
		__asm__ volatile("fcmeq %0.2d, %4.2d, %4.2d\n\tmvn %0.16b, %0.16b\n\tneg %1.2d, %0.2d\n\tmov %2.16b, %0.16b\n\tbsl %2.16b, %5.16b, %4.16b\n\taddp d31, %1.2d\n\tfmov %3, d31"
			: "=&w"(m), "=&w"(cnt), "=&w"(out), "=r"(n) : "w"(x), "w"(repl) : "v31");
		P("nan mask", m); P("nan neg", cnt); P("nan bsl", out);
		printf("nan count %lu\n", (unsigned long)n);
		uint8x16_t q = vdupq_n_u8(0x3c), r8;
		OPAQUE(q);
		__asm__("mvn %0.8b, %1.8b" : "=w"(r8) : "w"(q));
		P("mvn 8b", r8);
		__asm__("eor %0.8b, %1.8b, %1.8b" : "=w"(r8) : "w"(q));
		P("eor 8b", r8);
	}
	uint8_t bmem[16] = { 0 };
	vst1q_lane_u8(&bmem[3], vdupq_n_u8(0x7e), 9);
	printf("st1 lane b: %x %x %x\n", bmem[2], bmem[3], bmem[4]);

	unsigned long hw = getauxval(AT_HWCAP);
	// (Only the features both qemu and wasm-env report: qemu also claims SVE
	// and AES, which wasm-env deliberately doesn't.)
	printf("hwcap: fp=%lu asimd=%lu\n", hw & 1, (hw >> 1) & 1);
	return 0;
}
