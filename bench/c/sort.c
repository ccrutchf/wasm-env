// Quicksort of random 32-bit integers: mixed compute and memory access.
#include "bench.h"

#define N (4u << 20)

static uint32_t *data;

void setup(void) {
  data = bench_alloc(sizeof(uint32_t) * N);
  uint64_t s = 3;
  for (uint32_t i = 0; i < N; i++)
    data[i] = (uint32_t)rng_next(&s);
}

static void insertion(uint32_t *v, long n) {
  for (long i = 1; i < n; i++) {
    uint32_t x = v[i];
    long j = i - 1;
    while (j >= 0 && v[j] > x) {
      v[j + 1] = v[j];
      j--;
    }
    v[j + 1] = x;
  }
}

static void quicksort(uint32_t *v, long n) {
  while (n > 16) {
    uint32_t a = v[0], b = v[n / 2], c = v[n - 1];
    uint32_t pivot = a < b ? (b < c ? b : (a < c ? c : a)) : (a < c ? a : (b < c ? c : b));
    long i = 0, j = n - 1;
    for (;;) {
      while (v[i] < pivot)
        i++;
      while (v[j] > pivot)
        j--;
      if (i >= j)
        break;
      uint32_t t = v[i];
      v[i] = v[j];
      v[j] = t;
      i++;
      j--;
    }
    // Recurse into the smaller half to bound stack depth.
    if (j + 1 < n - j - 1) {
      quicksort(v, j + 1);
      v += j + 1;
      n -= j + 1;
    } else {
      quicksort(v + j + 1, n - j - 1);
      n = j + 1;
    }
  }
  insertion(v, n);
}

uint64_t run(void) {
  quicksort(data, N);
  uint64_t sum = 0;
  for (uint32_t i = 0; i < N; i += 4099)
    sum = sum * 31 + data[i];
  for (uint32_t i = 1; i < N; i++)
    if (data[i - 1] > data[i])
      return 0; // not sorted
  return sum;
}
