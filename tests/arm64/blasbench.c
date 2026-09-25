// OpenBLAS (numpy's bundled build) called directly: checksums on stdout (to
// compare with qemu), timings on stderr. For measuring SIMD translation.
//   blasbench LIBRARY.so [N]
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

typedef void dgemm_fn(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t, double, const double *, int64_t,
                      const double *, int64_t, double, double *, int64_t);
typedef void sgemm_fn(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t, float, const float *, int64_t,
                      const float *, int64_t, float, float *, int64_t);
typedef double ddot_fn(int64_t, const double *, int64_t, const double *, int64_t);
enum { RowMajor = 101, NoTrans = 111 };

static double now(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return t.tv_sec + t.tv_nsec * 1e-9;
}

int main(int argc, char **argv)
{
	void *lib = dlopen(argv[1], RTLD_NOW);
	if (!lib) {
		printf("dlopen: %s\n", dlerror());
		return 1;
	}
	dgemm_fn *dgemm = (dgemm_fn *)dlsym(lib, "scipy_cblas_dgemm64_");
	sgemm_fn *sgemm = (sgemm_fn *)dlsym(lib, "scipy_cblas_sgemm64_");
	ddot_fn *ddot = (ddot_fn *)dlsym(lib, "scipy_cblas_ddot64_");
	int n = argc > 2 ? atoi(argv[2]) : 200;
	double *a = malloc(sizeof(double) * n * n), *b = malloc(sizeof(double) * n * n), *c = malloc(sizeof(double) * n * n);
	float *af = malloc(sizeof(float) * n * n), *bf = malloc(sizeof(float) * n * n), *cf = malloc(sizeof(float) * n * n);
	for (int i = 0; i < n * n; i++) {
		a[i] = (i % 17) * 0.25 - 2;
		b[i] = (i % 13) * 0.5 - 3;
		af[i] = (float)a[i];
		bf[i] = (float)b[i];
	}
	double t = now();
	dgemm(RowMajor, NoTrans, NoTrans, n, n, n, 1.0, a, n, b, n, 0.0, c, n);
	double dt = now() - t;
	double sum = 0;
	for (int i = 0; i < n * n; i++)
		sum += c[i] * ((i % 7) + 1);
	printf("dgemm %d: %.6f\n", n, sum);
	t = now();
	sgemm(RowMajor, NoTrans, NoTrans, n, n, n, 1.0f, af, n, bf, n, 0.0f, cf, n);
	double st = now() - t;
	double fsum = 0;
	for (int i = 0; i < n * n; i++)
		fsum += cf[i] * ((i % 7) + 1);
	printf("sgemm %d: %.3f\n", n, fsum);
	t = now();
	double d = 0;
	for (int r = 0; r < 100; r++)
		d += ddot(n * n, a, 1, b, 1);
	double ddt = now() - t;
	printf("ddot: %.6f\n", d);
	fprintf(stderr, "dgemm %.1f ms (%.2f GFLOPS), sgemm %.1f ms, 100x ddot %.1f ms\n", dt * 1e3,
	        2.0 * n * n * n / dt * 1e-9, st * 1e3, ddt * 1e3);
	return 0;
}
