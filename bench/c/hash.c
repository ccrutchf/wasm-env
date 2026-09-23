// Open-addressing hash table: random access into a large table.
#include "bench.h"

#define SLOTS (1u << 22)
#define INSERTS (2u << 20)
#define LOOKUPS (8u << 20)

static uint64_t *keys, *vals;

void setup(void) {
  keys = bench_alloc(sizeof(uint64_t) * SLOTS);
  vals = bench_alloc(sizeof(uint64_t) * SLOTS);
}

static inline uint32_t slot_of(uint64_t k) {
  return (uint32_t)((k * 0x9E3779B97F4A7C15ULL) >> 42);
}

uint64_t run(void) {
  uint64_t s = 4;
  for (uint32_t i = 0; i < INSERTS; i++) {
    uint64_t k = rng_next(&s) | 1;
    uint32_t h = slot_of(k);
    while (keys[h] && keys[h] != k)
      h = (h + 1) & (SLOTS - 1);
    keys[h] = k;
    vals[h] += i;
  }
  // Even i replays the inserted keys (hits); odd i uses a fresh stream (misses).
  uint64_t hit = 4, miss = 5, sum = 0;
  for (uint32_t i = 0; i < LOOKUPS; i++) {
    if (!(i & 1) && (i / 2) % INSERTS == 0)
      hit = 4; // restart the inserted sequence
    uint64_t k = ((i & 1) ? rng_next(&miss) : rng_next(&hit)) | 1;
    uint32_t h = slot_of(k);
    while (keys[h] && keys[h] != k)
      h = (h + 1) & (SLOTS - 1);
    sum += keys[h] ? vals[h] : 1;
  }
  return sum;
}
