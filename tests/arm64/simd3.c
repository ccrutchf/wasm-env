// More NEON and scalar instructions that libraries (zlib, OpenSSL, SQLite,
// Python extensions) use: vector orr/bic immediates, compares with zero,
// float widening/narrowing, shift-and-insert, widening multiplies (also by
// element), post-index-by-register loads/stores, eor3 and crc32.
#include <arm_acle.h>
#include <arm_neon.h>
#include <stdint.h>
#include <stdio.h>

#define OPAQUE(v) __asm__("" : "+w"(v))

static void print_v(const char *name, uint8x16_t v)
{
	uint8_t b[16];
	vst1q_u8(b, v);
	printf("%s:", name);
	for (int i = 0; i < 16; i++) printf(" %02x", b[i]);
	printf("\n");
}
#define P(name, v) print_v(name, vreinterpretq_u8_u32(vreinterpretq_u32_u8((uint8x16_t)(v))))

int main(void)
{
	uint32x4_t a = { 0x11223344, 0x80000001, 0x7fffffff, 0 };
	int32x4_t s = { -5, 0, 7, -2147483647 - 1 };
	OPAQUE(a);
	OPAQUE(s);
	uint32x4_t r = a;
	__asm__("bic %0.4s, #0x80, lsl #24" : "+w"(r));
	P("bic imm", r);
	r = a;
	__asm__("orr %0.4s, #0x12, lsl #8" : "+w"(r));
	P("orr imm", r);
	uint16x4_t h = { 1, 2, 3, 4 };
	OPAQUE(h);
	__asm__("orr %0.4h, #0xa0" : "+w"(h));
	P("orr imm .4h", vcombine_u16(h, vdup_n_u16(0)));
	P("cmeq #0", vceqzq_s32(s));
	P("cmge #0", vcgezq_s32(s));
	P("cmgt #0", vcgtzq_s32(s));
	P("cmle #0", vclezq_s32(s));
	P("cmlt #0", vcltzq_s32(s));
	int64_t sd = -3, zd;
	__asm__("cmge %d0, %d1, #0" : "=w"(zd) : "w"(sd));
	printf("cmge d #0: %lld\n", (long long)zd);

	float32x4_t f = { 1.5f, -2.25f, 3.0e30f, 1.0e-30f };
	OPAQUE(f);
	float64x2_t lo = vcvt_f64_f32(vget_low_f32(f)), hi = vcvt_high_f64_f32(f);
	printf("fcvtl: %g %g fcvtl2: %g %g\n", lo[0], lo[1], hi[0], hi[1]);
	float64x2_t dd = { 1.0 / 3.0, 1e300 };
	OPAQUE(dd);
	float32x2_t n = vcvt_f32_f64(dd);
	float32x4_t n2 = vcvt_high_f32_f64(n, dd);
	printf("fcvtn: %.9g %g fcvtn2: %.9g %g %.9g %g\n", (double)n[0], (double)n[1], (double)n2[0], (double)n2[1],
	       (double)n2[2], (double)n2[3]);

	P("sli #12", vsliq_n_u32(a, vdupq_n_u32(0x12345678), 12));
	P("sri #20", vsriq_n_u32(a, vdupq_n_u32(0x87654321), 20));
	P("sli .16b #3", vsliq_n_u8((uint8x16_t)a, vdupq_n_u8(0xff), 3));

	uint32x2_t x2 = vget_low_u32(a), y2 = { 0xffffffffu, 3 };
	OPAQUE(y2);
	P("umull", vmull_u32(x2, y2));
	P("umull2", vmull_high_u32(a, vcombine_u32(y2, y2)));
	P("smull lane", vmull_lane_s32(vget_low_s32(s), vreinterpret_s32_u32(y2), 1));
	P("umlal lane", vmlal_lane_u32(vdupq_n_u64(10), x2, y2, 0));
	P("smlsl", vmlsl_s16(vdupq_n_s32(1000), vdup_n_s16(-7), vdup_n_s16(9)));
	P("umlal2 .8h", vmlal_high_u8(vdupq_n_u16(1), (uint8x16_t)a, vdupq_n_u8(200)));

	uint8_t buf[64], out[64];
	for (int i = 0; i < 64; i++) buf[i] = (uint8_t)(i * 13);
	const uint8_t *p = buf;
	uint8_t *q = out;
	long step = 16;
	uint8x16_t v;
	for (int i = 0; i < 4; i++) {
		__asm__ volatile("ld1 {%0.16b}, [%1], %2" : "=w"(v), "+r"(p) : "r"(step));
		__asm__ volatile("st1 {%1.16b}, [%0], %2" : "+r"(q) : "w"(v), "r"(step) : "memory");
	}
	printf("ld1/st1 post-index reg: %d %ld\n", __builtin_memcmp(buf, out, 64) == 0, (long)(p - buf));

	// Shift by register: the low byte of each lane is a signed count, and
	// negative counts shift right (as vectorized byte extraction relies on).
	int64x2_t cnt64 = { -8, 3 }, big64 = { -70, 70 };
	int32x4_t cnt32 = { -1, -31, 5, -40 };
	int8x16_t cnt8 = { -1, -7, -8, 0, 1, 7, 8, -128, 127, -3, 3, -9, 9, 2, -2, 0 };
	OPAQUE(cnt64);
	OPAQUE(cnt32);
	OPAQUE(cnt8);
	uint64x2_t w64 = { 0x8123456789abcdefull, 0xfedcba9876543210ull };
	OPAQUE(w64);
	P("ushl.2d", vshlq_u64(w64, cnt64));
	P("sshl.2d", vshlq_s64(vreinterpretq_s64_u64(w64), cnt64));
	P("ushl.2d big", vshlq_u64(w64, big64));
	P("sshl.2d big", vshlq_s64(vreinterpretq_s64_u64(w64), big64));
	P("ushl.4s", vshlq_u32(vreinterpretq_u32_u64(w64), cnt32));
	P("sshl.4s", vshlq_s32(vreinterpretq_s32_u64(w64), cnt32));
	P("ushl.16b", vshlq_u8(vreinterpretq_u8_u64(w64), cnt8));
	P("sshl.16b", vshlq_s8(vreinterpretq_s8_u64(w64), cnt8));

	uint64x2_t e1 = { 0x0123456789abcdefull, 0xfedcba9876543210ull }, e2 = { 0xffff0000ffff0000ull, 1 },
		   e3 = { 42, 0x8000000000000000ull };
	OPAQUE(e1);
	uint64x2_t e;
	__asm__("eor3 %0.16b, %1.16b, %2.16b, %3.16b" : "=w"(e) : "w"(e1), "w"(e2), "w"(e3));
	P("eor3", e);

	uint32_t c = 0xffffffff;
	c = __crc32b(c, 'a');
	c = __crc32h(c, 0x6362);
	c = __crc32w(c, 0x67666564);
	c = __crc32d(c, 0x6f6e6d6c6b6a6968ull);
	printf("crc32: %08x\n", ~c);
	uint32_t cc = __crc32cd(__crc32cw(__crc32ch(__crc32cb(0xffffffff, 1), 2), 3), 4);
	printf("crc32c: %08x\n", ~cc);
	return 0;
}
