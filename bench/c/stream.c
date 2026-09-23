// Streaming loads and stores over large arrays (STREAM "triad"-style).
// Dominated by memory accesses, so it shows bounds-check cost.
#include "bench.h"

#define N (8u << 20)
#define REPS 10

static uint32_t *a, *b, *c;

void setup(void) {
  a = bench_alloc(sizeof(uint32_t) * N);
  b = bench_alloc(sizeof(uint32_t) * N);
  c = bench_alloc(sizeof(uint32_t) * N);
  for (uint32_t i = 0; i < N; i++) {
    a[i] = i;
    b[i] = i ^ 0x5555;
  }
}

uint64_t run(void) {
  uint64_t sum = 0;
  for (uint32_t r = 0; r < REPS; r++) {
    for (uint32_t i = 0; i < N; i++)
      c[i] = a[i] + b[i] * (r + 3);
    for (uint32_t i = 0; i < N; i++)
      a[i] = c[i] >> 1;
    sum += a[r * 997];
  }
  return sum;
}
