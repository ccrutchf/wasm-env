// NEON that image libraries use (pixman, libjpeg): absolute difference
// long, narrowing high halves (addhn, subhn and their rounding forms),
// rounding shift right narrow, saturating doubling multiplies (vector and
// by element), vector clz and cls. Differential against qemu.
#include <arm_neon.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static void p8(const char *name, uint8x16_t v) {
    uint8_t a[16];
    vst1q_u8(a, v);
    printf("%s:", name);
    for (int k = 0; k < 16; k++) printf(" %u", a[k]);
    printf("\n");
}

static void p16(const char *name, uint16x8_t v) {
    uint16_t a[8];
    vst1q_u16(a, v);
    printf("%s:", name);
    for (int k = 0; k < 8; k++) printf(" %u", a[k]);
    printf("\n");
}

static void p32(const char *name, uint32x4_t v) {
    uint32_t a[4];
    vst1q_u32(a, v);
    printf("%s:", name);
    for (int k = 0; k < 4; k++) printf(" %u", a[k]);
    printf("\n");
}

// Inputs the compiler can't see through (or it computes the results).
uint8_t a8[16], b8[16];
int16_t a16[8], b16[8];
int32_t a32[4], b32[4];

int main(void) {
    const uint8_t ia8[16] = {0, 1, 2, 3, 100, 200, 255, 128, 7, 250, 17, 0, 255, 64, 33, 99};
    const uint8_t ib8[16] = {255, 0, 3, 3, 200, 100, 0, 127, 70, 5, 17, 255, 1, 192, 44, 11};
    const int16_t ia16[8] = {-32768, 32767, -1, 1000, -1000, 12345, -32768, 7};
    const int16_t ib16[8] = {-32768, 32767, 1, -1000, 1000, 23456, 32767, -9};
    const int32_t ia32[4] = {INT32_MIN, INT32_MAX, -123456, 987654321};
    const int32_t ib32[4] = {INT32_MIN, 2, 654321, -123456789};
    memcpy(a8, ia8, 16), memcpy(b8, ib8, 16), memcpy(a16, ia16, 16), memcpy(b16, ib16, 16);
    memcpy(a32, ia32, 16), memcpy(b32, ib32, 16);
    __asm__ volatile("" : : : "memory");
    uint8x16_t ua = vld1q_u8(a8), ub = vld1q_u8(b8);
    int16x8_t sa = vld1q_s16(a16), sb = vld1q_s16(b16);
    int32x4_t la = vld1q_s32(a32), lb = vld1q_s32(b32);

    // Absolute difference long, and accumulate.
    p16("uabdl", vabdl_u8(vget_low_u8(ua), vget_low_u8(ub)));
    p16("uabdl2", vabdl_high_u8(ua, ub));
    p32("sabdl", vreinterpretq_u32_s32(vabdl_s16(vget_low_s16(sa), vget_low_s16(sb))));
    p32("sabdl2", vreinterpretq_u32_s32(vabdl_high_s16(sa, sb)));
    uint16x8_t acc = vdupq_n_u16(65000);
    p16("uabal", vabal_u8(acc, vget_low_u8(ua), vget_low_u8(ub)));
    p32("sabal2", vreinterpretq_u32_s32(vabal_high_s16(vdupq_n_s32(-5), sa, sb)));

    // Narrow to the high half, plain and rounded, low and high halves.
    uint16x8_t wa = vreinterpretq_u16_s16(sa), wb = vreinterpretq_u16_s16(sb);
    uint8x8_t lo = vaddhn_u16(wa, wb);
    p8("addhn/addhn2", vaddhn_high_u16(lo, wb, wa));
    p8("raddhn/raddhn2", vraddhn_high_u16(vraddhn_u16(wa, wb), wa, vdupq_n_u16(0x7f)));
    p8("subhn/rsubhn2", vrsubhn_high_u16(vsubhn_u16(wa, wb), wb, wa));
    p16("rsubhn32", vcombine_u16(vrsubhn_u32(vreinterpretq_u32_s32(la), vreinterpretq_u32_s32(lb)), vdup_n_u16(1)));

    // Rounding shift right narrow.
    p8("rshrn", vcombine_u8(vrshrn_n_u16(wa, 3), vrshrn_n_u16(wb, 8)));
    p8("rshrn2", vrshrn_high_n_u16(vrshrn_n_u16(wa, 1), wb, 7));
    p16("rshrn32", vcombine_u16(vrshrn_n_u32(vreinterpretq_u32_s32(la), 16), vrshrn_n_u32(vreinterpretq_u32_s32(lb), 5)));

    // Saturating doubling multiply high: min * min saturates.
    p16("sqdmulh", vreinterpretq_u16_s16(vqdmulhq_s16(sa, sb)));
    p16("sqrdmulh", vreinterpretq_u16_s16(vqrdmulhq_s16(sa, sb)));
    p16("sqdmulh[1]", vreinterpretq_u16_s16(vqdmulhq_laneq_s16(sa, sb, 1)));
    p16("sqrdmulh[6]", vreinterpretq_u16_s16(vqrdmulhq_laneq_s16(sa, sb, 6)));
    p32("sqdmulh.4s", vreinterpretq_u32_s32(vqdmulhq_s32(la, lb)));
    p32("sqrdmulh.4s[3]", vreinterpretq_u32_s32(vqrdmulhq_laneq_s32(la, lb, 3)));
    p16("sqdmulh.4h", vcombine_u16(vreinterpret_u16_s16(vqdmulh_s16(vget_low_s16(sa), vget_high_s16(sb))), vdup_n_u16(9)));

    // Leading zeros and leading sign bits.
    p8("clz.16b", vclzq_u8(ua));
    p16("clz.8h", vclzq_u16(wa));
    p32("clz.4s", vclzq_u32(vreinterpretq_u32_s32(la)));
    p16("cls.8h", vreinterpretq_u16_s16(vclsq_s16(sa)));
    p8("cls.16b", vreinterpretq_u8_s8(vclsq_s8(vreinterpretq_s8_u8(ub))));
    p32("cls.4s", vreinterpretq_u32_s32(vclsq_s32(lb)));

    // Halving add and subtract, rounded or not.
    p8("uhadd", vhaddq_u8(ua, ub));
    p8("urhadd", vrhaddq_u8(ua, ub));
    p8("uhsub", vhsubq_u8(ua, ub));
    p16("shadd", vreinterpretq_u16_s16(vhaddq_s16(sa, sb)));
    p16("srhadd", vreinterpretq_u16_s16(vrhaddq_s16(sa, sb)));
    p16("shsub", vreinterpretq_u16_s16(vhsubq_s16(sa, sb)));

    // Rounding shift right, and accumulate.
    p16("urshr", vrshrq_n_u16(wa, 3));
    p16("srshr", vreinterpretq_u16_s16(vrshrq_n_s16(sa, 16)));
    p8("ursra", vrsraq_n_u8(ua, ub, 1));
    p32("srsra", vreinterpretq_u32_s32(vrsraq_n_s32(la, lb, 7)));

    // Saturating (rounding) shift right narrow, low and high halves.
    p8("sqshrn sqshrn2", vreinterpretq_u8_s8(vqshrn_high_n_s16(vqshrn_n_s16(sa, 2), sb, 7)));
    p16("sqrshrn sqrshrn2", vreinterpretq_u16_s16(vqrshrn_high_n_s32(vqrshrn_n_s32(la, 3), lb, 16)));
    p8("sqshrun sqrshrun2", vqrshrun_high_n_s16(vqshrun_n_s16(sa, 4), sb, 1));
    p16("uqshrn uqrshrn2", vqrshrn_high_n_u32(vqshrn_n_u32(vreinterpretq_u32_s32(la), 12), vreinterpretq_u32_s32(lb), 1));

    // Saturating shift left to unsigned.
    p8("uqshl", vqshlq_n_u8(ua, 1));
    p16("sqshlu", vqshluq_n_s16(sa, 3));

    // Load and replicate structures.
    uint8x16x3_t three = vld3q_dup_u8(a8 + 5);
    p8("ld3r.0", three.val[0]);
    p8("ld3r.2", three.val[2]);
    uint16x8x2_t two = vld2q_dup_u16((const uint16_t *)a16 + 3);
    p16("ld2r.1", two.val[1]);
    uint32x4x4_t four = vld4q_dup_u32((const uint32_t *)a32);
    p32("ld4r.3", four.val[3]);
    return 0;
}
