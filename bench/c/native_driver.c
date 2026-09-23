// Native driver: runs setup()/run() with a fresh arena each iteration,
// mirroring the wasm harness (fresh instance per iteration).
//
// Output, one line per run: "<nanoseconds> <checksum>"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

extern unsigned char *bench_arena;
extern size_t bench_arena_used;
void setup(void);
uint64_t run(void);

#define ARENA_BYTES ((size_t)1 << 30)

static uint64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (uint64_t)ts.tv_sec * 1000000000u + (uint64_t)ts.tv_nsec;
}

int main(int argc, char **argv) {
  int runs = argc > 1 ? atoi(argv[1]) : 5;
  bench_arena = malloc(ARENA_BYTES);
  if (!bench_arena)
    return 1;
  for (int i = 0; i < runs; i++) {
    // Start each run from zeroed memory, like a fresh wasm instance.
    memset(bench_arena, 0, bench_arena_used);
    bench_arena_used = 0;
    setup();
    if (bench_arena_used > ARENA_BYTES)
      return 2;
    uint64_t t0 = now_ns();
    uint64_t sum = run();
    uint64_t t1 = now_ns();
    printf("%llu %llu\n", (unsigned long long)(t1 - t0), (unsigned long long)sum);
  }
  return 0;
}
