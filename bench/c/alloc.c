// Bump allocator. On wasm it grows linear memory; natively it carves from
// an arena the driver provides.
#include "bench.h"

#ifdef __wasm__

extern unsigned char __heap_base;
static uintptr_t heap_top = (uintptr_t)&__heap_base;

void *bench_alloc(size_t bytes) {
  uintptr_t p = (heap_top + 63) & ~(uintptr_t)63;
  uintptr_t end = p + bytes;
  size_t have = __builtin_wasm_memory_size(0) * 65536;
  if (end > have) {
    size_t pages = (end - have + 65535) / 65536;
    if (__builtin_wasm_memory_grow(0, pages) == (size_t)-1)
      __builtin_trap();
  }
  heap_top = end;
  return (void *)p;
}

// The compiler may emit calls to these even with -fno-builtin.
void *memset(void *d, int c, size_t n) {
  unsigned char *p = d;
  while (n--)
    *p++ = (unsigned char)c;
  return d;
}

void *memcpy(void *d, const void *s, size_t n) {
  unsigned char *p = d;
  const unsigned char *q = s;
  while (n--)
    *p++ = *q++;
  return d;
}

#else

unsigned char *bench_arena;
size_t bench_arena_used;

void *bench_alloc(size_t bytes) {
  size_t p = (bench_arena_used + 63) & ~(size_t)63;
  bench_arena_used = p + bytes;
  return bench_arena + p;
}

#endif
