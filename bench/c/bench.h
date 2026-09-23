// Shared support for freestanding benchmarks.
//
// Each benchmark defines setup() (untimed: allocate and fill data) and
// run() (timed: returns a checksum). The same source compiles to native,
// wasm32 and wasm64, so no libc is used.
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __wasm__
#define EXPORT(name) __attribute__((export_name(#name)))
#else
#define EXPORT(name)
#endif

void *bench_alloc(size_t bytes);

// Deterministic PRNG (xorshift64*), identical on every target.
static inline uint64_t rng_next(uint64_t *s) {
  uint64_t x = *s;
  x ^= x >> 12;
  x ^= x << 25;
  x ^= x >> 27;
  *s = x;
  return x * 0x2545F4914F6CDD1DULL;
}

EXPORT(setup) void setup(void);
EXPORT(run) uint64_t run(void);
