// Driver for a benchmark translated by wasm2c and compiled with clang.
// Same protocol as c/native_driver.c: fresh instance per run, setup()
// untimed, run() timed, one "<nanoseconds> <checksum>" line per run.
//
// Build with -DMOD=<bench> and -include <bench>.h.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#include "wasm-rt.h"

#define CAT_(a, b) a##b
#define CAT(a, b) CAT_(a, b)
#define CAT3(a, b, c) CAT(CAT(a, b), c)

#define INSTANCE CAT(w2c_, MOD)
#define INSTANTIATE CAT3(wasm2c_, MOD, _instantiate)
#define FREE CAT3(wasm2c_, MOD, _free)
#define SETUP CAT3(w2c_, MOD, _setup)
#define RUN CAT3(w2c_, MOD, _run)

static uint64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (uint64_t)ts.tv_sec * 1000000000u + (uint64_t)ts.tv_nsec;
}

int main(int argc, char** argv) {
  int runs = argc > 1 ? atoi(argv[1]) : 5;
  wasm_rt_init();
  for (int i = 0; i < runs; i++) {
    INSTANCE inst;
    INSTANTIATE(&inst);
    SETUP(&inst);
    uint64_t t0 = now_ns();
    uint64_t sum = RUN(&inst);
    uint64_t t1 = now_ns();
    FREE(&inst);
    printf("%llu %llu\n", (unsigned long long)(t1 - t0), (unsigned long long)sum);
  }
  wasm_rt_free();
  return 0;
}
