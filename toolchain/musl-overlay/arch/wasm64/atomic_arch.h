// wasm-env: version 0 is single-threaded and doesn't enable the WebAssembly
// threads feature, so the compiler lowers these to plain loads and stores.
#define a_cas a_cas
static inline int a_cas(volatile int *p, int t, int s)
{
	return __sync_val_compare_and_swap(p, t, s);
}

#define a_cas_p a_cas_p
static inline void *a_cas_p(volatile void *p, void *t, void *s)
{
	return __sync_val_compare_and_swap((void *volatile *)p, t, s);
}

#define a_barrier a_barrier
static inline void a_barrier()
{
	__sync_synchronize();
}

#define a_crash a_crash
static inline void a_crash()
{
	__builtin_trap();
}

#define a_ctz_64 a_ctz_64
static inline int a_ctz_64(uint64_t x)
{
	return __builtin_ctzll(x);
}

#define a_clz_64 a_clz_64
static inline int a_clz_64(uint64_t x)
{
	return __builtin_clzll(x);
}
