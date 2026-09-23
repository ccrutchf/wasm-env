// Inserted into wasm2c output right after MEMCHECK_GENERAL is defined
// (see the Makefile): replace bounds checks with index masking.
// Requires w2c/mem_masked.c for the fixed, never-moving reservation.
#undef MEMCHECK_GENERAL
#define MEMCHECK_GENERAL(mem, a, t) WASM_RT_CHECK_BASE(mem);
#undef MEM_ADDR
#define MEM_ADDR(mem, addr, n) \
  &((mem)->data[(addr) & ((1ull << WASM_ENV_MASK_BITS) - 1)])
