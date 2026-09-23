// Masked linear memory for wasm2c output (M-1 experiment).
//
// Replaces wasm2c's wasm-rt-mem-impl.c. Each memory reserves a fixed
// 2^WASM_ENV_MASK_BITS bytes plus a guard region, all inaccessible, and
// makes pages accessible as the memory grows. It never moves, so the
// masked address `data + (addr & mask)` always lands inside the
// reservation: in-bounds pages work, pages past the current size fault.
//
// Linux/macOS only (mmap). Shared memories are not supported.
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>

#include "wasm-rt.h"

#define RESERVATION (1ull << WASM_ENV_MASK_BITS)
// Covers the widest access (16 bytes for v128) past the end of the reservation.
#define GUARD (1ull << 16)

void wasm_rt_allocate_memory(wasm_rt_memory_t* memory,
                             uint64_t initial_pages,
                             uint64_t max_pages,
                             bool is64,
                             uint32_t page_size) {
  uint64_t max_bytes = RESERVATION / page_size;
  memory->page_size = page_size;
  memory->pages = initial_pages;
  memory->max_pages = max_pages < max_bytes ? max_pages : max_bytes;
  memory->size = initial_pages * page_size;
  memory->is64 = is64;

  uint8_t* data = mmap(NULL, RESERVATION + GUARD, PROT_NONE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  if (data == MAP_FAILED) {
    perror("mmap reservation");
    abort();
  }
  if (memory->size &&
      mprotect(data, memory->size, PROT_READ | PROT_WRITE) != 0) {
    perror("mprotect");
    abort();
  }
  memory->data = data;
  memory->data_end = data + RESERVATION;
}

uint64_t wasm_rt_grow_memory(wasm_rt_memory_t* memory, uint64_t delta) {
  uint64_t old_pages = memory->pages;
  uint64_t new_pages = old_pages + delta;
  if (new_pages < old_pages || new_pages > memory->max_pages)
    return (uint64_t)-1;
  uint64_t old_size = memory->size;
  uint64_t new_size = new_pages * memory->page_size;
  if (new_size > old_size &&
      mprotect(memory->data + old_size, new_size - old_size,
               PROT_READ | PROT_WRITE) != 0)
    return (uint64_t)-1;
  memory->pages = new_pages;
  memory->size = new_size;
  return old_pages;
}

void wasm_rt_free_memory(wasm_rt_memory_t* memory) {
  munmap(memory->data, RESERVATION + GUARD);
}
