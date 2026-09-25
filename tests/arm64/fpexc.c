// FPSR's cumulative exception flags as fenv.h reads them: invalid, divide by
// zero and overflow (the ones numpy warns about by default), from scalar and
// vector arithmetic and conversions.
#include <arm_neon.h>
#include <fenv.h>
#include <float.h>
#include <math.h>
#include <stdio.h>

#define WATCHED (FE_INVALID | FE_DIVBYZERO | FE_OVERFLOW)

static void show(const char *what)
{
	int f = fetestexcept(WATCHED);
	printf("%-16s%s%s%s%s\n", what, f & FE_INVALID ? " invalid" : "", f & FE_DIVBYZERO ? " divbyzero" : "",
	       f & FE_OVERFLOW ? " overflow" : "", f ? "" : " none");
	feclearexcept(FE_ALL_EXCEPT);
}

int main(void)
{
	volatile double one = 1.0, zero = 0.0, big = DBL_MAX, neg = -1.0, nan = NAN, inf = INFINITY;
	volatile double r;
	volatile float rf;
	volatile long l;
	volatile unsigned u;
	feclearexcept(FE_ALL_EXCEPT);
	r = one + one;             show("add");
	r = one / zero;            show("1/0");
	r = zero / zero;           show("0/0");
	r = big * 2.0;             show("max*2");
	r = sqrt(neg);             show("sqrt(-1)");
	r = nan + one;             show("nan+1");
	r = inf - inf;             show("inf-inf");
	r = inf * one;             show("inf*1");
	r = fma(big, 2.0, -one);   show("fma overflow");
	rf = (float)(big / 2);     show("to float");
	l = (long)nan;             show("nan to long");
	l = (long)1e30;            show("1e30 to long");
	u = (unsigned)neg;         show("-1 to unsigned");
	l = (long)-3.5;            show("-3.5 to long");
	float64x2_t v = { 1.0, 0.0 }, z = { 0.0, 0.0 };
	__asm__ volatile("" : "+w"(v), "+w"(z));
	v = vdivq_f64(v, z);
	__asm__ volatile("" : "+w"(v)); show("vector 1/0");
	float32x4_t fv = vdupq_n_f32(FLT_MAX);
	__asm__ volatile("" : "+w"(fv));
	fv = vmulq_f32(fv, fv);
	__asm__ volatile("" : "+w"(fv)); show("vector overflow");
	int32x4_t iv = vcvtq_s32_f32(vdupq_n_f32(NAN));
	__asm__ volatile("" : "+w"(iv)); show("vector nan->int");
	float32x2_t nv = vcvt_f32_f64(vdupq_n_f64(1e300));
	__asm__ volatile("" : "+w"(nv)); show("vector narrow");
	(void)r; (void)rf; (void)l; (void)u;
	return 0;
}
