// Support code linked into every translated program.
//
// The lifted code (translator/src/lift) keeps guest state in thread-local
// globals and calls into here for program startup, system calls and fatal
// errors. This file is compiled for wasm64 without libc; it only uses the
// linux.syscall import (docs/abi.md).
#include <stdint.h>

long __wasm_env_host_syscall(long, long, long, long, long, long, long)
	__attribute__((__import_module__("linux"), __import_name__("syscall")));

typedef void (*block_fn)(long);

// Guest state: one block per thread, which the lifted code finds through
// the WebAssembly global __wasm_env_state (set by __wasm_env_set_state,
// defined with it in the lifted data module). Its layout must match the
// translator's STATE_POINTERS (translator/src/lift/mod.rs).
//
// The dispatch cache is a direct-mapped cache of region lookups, tagged with
// region number + 1 so the zeroed initial entries match nothing. The lifted
// code probes it inline at every indirect branch (see Block::jump_to) and
// calls `dispatch` only on a miss; the layout and hash must match. With
// WASM_ENV_DEBUG=1 nothing is cached, so every indirect branch comes through
// `dispatch` and is recorded for error reports.
#define DISPATCH_CACHE 4096
struct dispatch_entry {
	uint64_t tag;
	block_fn fn;
};
struct guest_state {
	uint64_t r[32];   // x0–x30, SP
	uint8_t f[4];     // N, Z, C, V
	uint32_t fpsr;
	uint64_t tp;      // TPIDR_EL0
	uint64_t monitor; // the value the last load-exclusive read
	__uint128_t v[32];
	struct dispatch_entry cache[DISPATCH_CACHE];
	uint64_t modules_seen; // see dispatch
	// A signal to deliver when the current syscall returns, as the runtime
	// fills it in (runtime/src/syscall.rs, deliver_to_guest); sig 0: none.
	struct signal_delivery {
		uint64_t sig, handler, flags, restorer, old_mask, alt_sp, alt_flags, alt_size;
	} signal;
	// The code epoch this thread's dispatch cache is from (see
	// __wasm_env_code_epoch).
	uint64_t code_epoch;
};
_Static_assert(__builtin_offsetof(struct guest_state, f) == 256, "STATE_POINTERS");
_Static_assert(__builtin_offsetof(struct guest_state, fpsr) == 260, "STATE_POINTERS");
_Static_assert(__builtin_offsetof(struct guest_state, tp) == 264, "STATE_POINTERS");
_Static_assert(__builtin_offsetof(struct guest_state, monitor) == 272, "STATE_POINTERS");
_Static_assert(__builtin_offsetof(struct guest_state, v) == 288, "STATE_POINTERS");
_Static_assert(__builtin_offsetof(struct guest_state, cache) == 800, "STATE_POINTERS");
_Static_assert(__builtin_offsetof(struct guest_state, signal) == 66344, "SIGNAL_DELIVERY in syscall.rs");
static _Thread_local struct guest_state guest;
void __wasm_env_set_state(struct guest_state *);
#define R guest.r
#define F guest.f
#define V guest.v
#define TP guest.tp
#define __wasm_env_dispatch_cache guest.cache


// The guest image, emitted by the lifter.
struct seg { uint64_t vaddr, filesz, memsz; const uint8_t *data; };
extern const struct seg guest_segments[];
extern const uint64_t guest_segment_count, guest_entry, guest_phdr, guest_phnum;
// The program's own entry (AT_ENTRY) and, when a translated dynamic linker
// runs first (guest_entry is then its entry), its load address (AT_BASE).
extern const uint64_t guest_app_entry, guest_interp_base;

// The region table, defined by the lifted data module: the sorted start
// addresses of the translated regions (REGION_SPAN bytes of guest code each)
// and their functions. `dispatch` maps a guest address to its region's
// function, which enters at the block there (or reports a missed target if
// there's none); the lifted code routes indirect branches and returns
// through it.
extern const uint64_t dispatch_addrs[];
extern void *const dispatch_fns[];
extern const uint64_t dispatch_count;

void dispatch(long pc);

enum { SYS_write = 64, SYS_exit_group = 94, SYS_clone = 220, ENOSYS = 38 };

static void put(const char *s, unsigned long n)
{
	__wasm_env_host_syscall(SYS_write, 2, (long)s, (long)n, 0, 0, 0);
}

static unsigned long len(const char *s)
{
	unsigned long n = 0;
	while (s[n]) n++;
	return n;
}

static void put_hex(uint64_t v)
{
	char buf[18] = "0x";
	for (int i = 0; i < 16; i++)
		buf[2 + i] = "0123456789abcdef"[(v >> (60 - 4 * i)) & 15];
	put(buf, 18);
}

struct lib { const char *path; uint64_t base, end; };

// Every object in the image (program, libraries, dynamic linker), for naming
// addresses in error messages.
extern const struct lib guest_objects[];
extern const uint64_t guest_object_count;

// The most recent block addresses dispatched to, for error messages.
#define RECENT 64
uint64_t wasm_env_recent[RECENT];
uint64_t wasm_env_recent_count;

static void put_address(uint64_t pc)
{
	put_hex(pc);
	for (uint64_t k = 0; k < guest_object_count; k++) {
		const struct lib *o = &guest_objects[k];
		if (pc >= o->base && pc < o->end) {
			put(" (", 2);
			put(o->path, len(o->path));
			put(" + ", 3);
			put_hex(pc - o->base);
			put(")", 1);
		}
	}
}

// WASM_ENV_DEBUG=1 in the program's environment: report the recent blocks
// when it aborts, too.
static int debug;

static void print_recent(uint64_t count)
{
	put("wasm-env: most recent blocks, oldest first:\n", 45);
	uint64_t n = wasm_env_recent_count < count ? wasm_env_recent_count : count;
	for (uint64_t k = wasm_env_recent_count - n; k < wasm_env_recent_count; k++) {
		put("  ", 2);
		put_address(wasm_env_recent[k % RECENT]);
		put("\n", 1);
	}
}

_Noreturn static void die(const char *what, uint64_t pc)
{
	put("wasm-env: translated program: ", 30);
	put(what, len(what));
	put(" at ", 4);
	put_address(pc);
	put("\n", 1);
	print_recent(debug ? RECENT : 16);
	__builtin_trap();
}

void __wasm_env_fatal(const char *why, uint64_t pc)
{
	die(why, pc);
}

// wasm-env's private call for reporting a missed jump target (see the runtime's
// translate.rs): the next translation adds it as a block root.
#define WASM_ENV_MISSED_TARGET (1L << 32)

static int translate_code(uint64_t pc);
void dispatch(long pc);

void __wasm_env_bad_target(uint64_t pc)
{
	// Code the program generated at run time, not translated yet (or not
	// this entry into it): translate it and go on there.
	if (translate_code(pc)) {
		dispatch((long)pc);
		__builtin_unreachable();
	}
	int known = 0;
	for (uint64_t k = 0; k < guest_object_count; k++) {
		const struct lib *o = &guest_objects[k];
		if (pc >= o->base && pc < o->end) {
			__wasm_env_host_syscall(WASM_ENV_MISSED_TARGET, (long)pc, (long)o->path, (long)(pc - o->base), 0, 0, 0);
			known = 1;
		}
	}
	// Elsewhere (a library loaded as its own module): the runtime knows which.
	if (!known)
		__wasm_env_host_syscall(WASM_ENV_MISSED_TARGET, (long)pc, 0, 0, 0, 0, 0);
	die("jump to an address that wasn't translated", pc);
}

// The FPSR exceptions an FP result `r` implies, as the lifted code's slow
// path asks (translator/src/lift/mod.rs, Block::check_fp): invalid when the
// result is NaN and no input was, divide by zero for x / y with y zero and x
// finite and nonzero, overflow when it's infinite from finite inputs. `kind`
// is the number of inputs (x, y, z; unused ones are 0), plus 4 for a
// division. Values arrive widened to double, which keeps NaN, infinity and
// finiteness. Underflow and inexact aren't modelled.
enum { FPSR_IOC = 1, FPSR_DZC = 2, FPSR_OFC = 4 };

uint32_t __wasm_env_fp_flags(double x, double y, double z, double r, uint32_t kind)
{
	const double inf = __builtin_inf();
	if (r == r && __builtin_fabs(r) != inf)
		return 0; // finite: another result in the block was special
	double in[3] = { x, y, z };
	int in_nan = 0, in_inf = 0;
	for (uint32_t i = 0; i < (kind & 3); i++) {
		in_nan |= in[i] != in[i];
		in_inf |= __builtin_fabs(in[i]) == inf;
	}
	if (r != r)
		return in_nan ? 0 : FPSR_IOC;
	if ((kind & 4) && y == 0 && x == x && x != 0 && __builtin_fabs(x) != inf)
		return FPSR_DZC;
	return in_nan || in_inf ? 0 : FPSR_OFC;
}

// CNTVCT_EL0, the virtual counter: CLOCK_MONOTONIC in nanoseconds, so
// CNTFRQ_EL0 reads 1 GHz (as ARMv8.6 requires). Spin loops time out on it
// (OpenBLAS's idle threads go to sleep after 2^28 ticks), so it must move.
uint64_t __wasm_env_counter(void)
{
	struct {
		int64_t sec, nsec;
	} ts = {0, 0};
	__wasm_env_host_syscall(113 /* clock_gettime */, 1 /* CLOCK_MONOTONIC */, (long)&ts, 0, 0, 0, 0);
	return (uint64_t)ts.sec * 1000000000u + (uint64_t)ts.nsec;
}

// The CRC32* instructions: CRC-32 (or CRC-32C, by polynomial) of the low
// `bytes` bytes of `data`, least significant first.
uint32_t __wasm_env_crc32(uint32_t crc, uint64_t data, uint32_t bytes, uint32_t poly)
{
	for (uint32_t i = 0; i < bytes; i++) {
		crc ^= (uint8_t)(data >> (8 * i));
		for (int k = 0; k < 8; k++)
			crc = (crc >> 1) ^ (poly & -(crc & 1));
	}
	return crc;
}

// Must match the translator's REGION_SPAN (translator/src/lift/mod.rs).
#define REGION_SHIFT 12

// Libraries translated as modules of their own (docs/abi.md, "Libraries"):
// the runtime appends each to this directory when it loads it into the
// process (the guest addresses it covers and its region table), then
// publishes the new count. Each thread's instance of the program has
// instances of the modules it has seen (guest_state.modules_seen); a thread
// that reaches a newer one asks the runtime to catch up first.
#define MODULES 1024
struct module {
	uint64_t lo, hi;
	const uint64_t *addrs;
	void *const *fns;
	uint64_t count;
};
struct module __wasm_env_modules[MODULES];
uint64_t __wasm_env_module_count;
#define WASM_ENV_SYNC_MODULES ((1L << 32) + 3)

// Code a program generates at run time (a JIT's): when control reaches an
// address no module covers, the runtime translates the executable mapping
// holding it (or, for a missed block in code it already translated, that
// code again with the new entry) and adds the module to the directory; 0
// if it did. When the program unmaps or rewrites such code, the runtime
// empties its directory entry and bumps the epoch, and each thread clears
// its dispatch cache, which may name the old code, when it next sees that
// (after a system call, or in dispatch).
#define WASM_ENV_TRANSLATE_CODE ((1L << 32) + 4)
uint64_t __wasm_env_code_epoch;

static void check_code_epoch(void)
{
	uint64_t epoch = __atomic_load_n(&__wasm_env_code_epoch, __ATOMIC_ACQUIRE);
	if (guest.code_epoch != epoch) {
		for (int i = 0; i < DISPATCH_CACHE; i++)
			guest.cache[i].tag = 0;
		guest.code_epoch = epoch;
	}
}

static int translate_code(uint64_t pc)
{
	if (__wasm_env_host_syscall(WASM_ENV_TRANSLATE_CODE, (long)pc, 0, 0, 0, 0, 0) != 0)
		return 0;
	// The new module may replace one this thread's cache names.
	for (int i = 0; i < DISPATCH_CACHE; i++)
		guest.cache[i].tag = 0;
	return 1;
}

// The function of the region starting at `start` in a region table, or 0.
static block_fn lookup(const uint64_t *addrs, void *const *fns, uint64_t count, uint64_t start)
{
	uint64_t lo = 0, hi = count;
	while (lo < hi) {
		uint64_t mid = lo + (hi - lo) / 2;
		uint64_t a = addrs[mid];
		if (a == start)
			return (block_fn)fns[mid];
		if (a < start)
			lo = mid + 1;
		else
			hi = mid;
	}
	return 0;
}

// Signal handlers without a restorer return to SIGRETURN_PC, which dispatch
// takes as rt_sigreturn (see "Signal handlers" below).
#define SIGRETURN_PC 0xfffffffffffff000ull
enum { SYS_rt_sigreturn = 139 };
static void sigreturn(void);
// Where the guest continues after a syscall, when not after its svc (see
// __wasm_env_svc_at).
static _Thread_local uint64_t svc_redirect;

// Find `pc`'s region function and tail-call it (a WebAssembly return_call,
// so the host stack doesn't grow): from the cache, else by binary search of
// the program's region table or a library module's. An address in no
// region was never translated.
void dispatch(long pc)
{
	uint64_t target = (uint64_t)pc;
	if (target == SIGRETURN_PC) {
		sigreturn();
		__attribute__((musttail)) return dispatch((long)svc_redirect);
	}
	check_code_epoch();
	wasm_env_recent[wasm_env_recent_count++ % RECENT] = target;
	uint64_t region = target >> REGION_SHIFT;
	struct dispatch_entry *e = &__wasm_env_dispatch_cache[(region ^ (region >> 12)) % DISPATCH_CACHE];
	if (e->tag == region + 1) {
		block_fn f = e->fn;
		__attribute__((musttail)) return f(pc); // the region function enters at pc
	}
	uint64_t start = region << REGION_SHIFT;
	block_fn f = lookup(dispatch_addrs, dispatch_fns, dispatch_count, start);
	if (!f) {
		uint64_t n = __atomic_load_n(&__wasm_env_module_count, __ATOMIC_ACQUIRE);
		for (uint64_t k = 0; k < n; k++) {
			const struct module *m = &__wasm_env_modules[k];
			if (target < m->lo || target >= m->hi)
				continue;
			if (k >= guest.modules_seen) {
				__wasm_env_host_syscall(WASM_ENV_SYNC_MODULES, 0, 0, 0, 0, 0, 0);
				guest.modules_seen = n;
			}
			f = lookup(m->addrs, m->fns, m->count, start);
			break;
		}
	}
	if (!f) {
		if (translate_code(target))
			__attribute__((musttail)) return dispatch(pc);
		__wasm_env_bad_target(target);
	}
	if (!debug) {
		e->tag = region + 1;
		e->fn = f;
	}
	__attribute__((musttail)) return f(pc);
}

// Shared libraries the translated dynamic linker loads at run time. Each
// was translated for a fixed base, so its code exists as translated blocks
// there: either in this module (translated with the program, listed in
// guest_libs) or as a module of its own, which the runtime translates and
// loads when the dynamic linker maps the library (docs/abi.md,
// "Libraries"). When the dynamic linker maps one, place the mapping at that
// base; it then relocates the library as usual, and every address it
// computes matches the translation.
extern const struct lib guest_libs[];
extern const uint64_t guest_lib_count;

enum { SYS_openat = 56, SYS_close = 57, SYS_pread64 = 67, SYS_munmap = 215, SYS_mmap = 222 };
enum { MAP_FIXED = 0x10, MAP_ANONYMOUS = 0x20 };
// wasm-env's call to load a library's module: (fd, where to write its base
// and end). Returns 0, or -errno if the file isn't a library it translates.
#define WASM_ENV_LOAD_LIBRARY ((1L << 32) + 2)

// Libraries placed so far: guest_libs first, then those the runtime loads.
#define LIBS 1024
static struct { uint64_t base, end; } libs[LIBS];
static uint64_t lib_count;

// For each file descriptor: 0 not yet known, NOT_LIB, or 1 + its library.
#define LIB_FDS 1024
#define NOT_LIB 0xffff
static uint16_t fd_lib[LIB_FDS];

// The file most recently opened (1 + fd), until it's mapped. glibc maps a
// library whose segments are aligned beyond the page size by first
// reserving an anonymous, inaccessible region, then mapping the file at an
// aligned address inside it; that reservation must be the library's base.
static long pending_fd;

// Reservations made that way: unmapping parts of them is a no-op.
#define RESERVATIONS 256
static struct { uint64_t start, end; } reserved[RESERVATIONS];
static uint64_t reservation_count;

static int same(const char *a, const char *b)
{
	while (*a && *a == *b) a++, b++;
	return *a == *b;
}

static const char *base_name(const char *p)
{
	const char *b = p;
	for (; *p; p++)
		if (*p == '/') b = p + 1;
	return b;
}

// The library a path names among those translated with the program: the
// exact path the translator found, or else the same file name (the dynamic
// linker may reach it through another directory, such as one from
// LD_LIBRARY_PATH).
static long lib_for_path(const char *path)
{
	for (uint64_t k = 0; k < guest_lib_count; k++)
		if (same(path, guest_libs[k].path)) return (long)k;
	for (uint64_t k = 0; k < guest_lib_count; k++)
		if (same(base_name(path), base_name(guest_libs[k].path))) return (long)k;
	return -1;
}

// The library open as `fd` (its index in libs), or -1: asks the runtime
// the first time, which loads the library's module if it is one.
static long lib_of_fd(long fd)
{
	if (fd < 0 || fd >= LIB_FDS || fd_lib[fd] == NOT_LIB)
		return -1;
	if (!fd_lib[fd]) {
		uint64_t range[2];
		if (lib_count == LIBS || __wasm_env_host_syscall(WASM_ENV_LOAD_LIBRARY, fd, (long)range, 0, 0, 0, 0) != 0) {
			fd_lib[fd] = NOT_LIB;
			return -1;
		}
		libs[lib_count].base = range[0];
		libs[lib_count].end = range[1];
		fd_lib[fd] = (uint16_t)++lib_count;
	}
	return fd_lib[fd] - 1;
}

static int in_a_lib(uint64_t addr)
{
	for (uint64_t k = 0; k < lib_count; k++)
		if (addr >= libs[k].base && addr < ((libs[k].end + 4095) & ~(uint64_t)4095)) return 1;
	for (uint64_t k = 0; k < reservation_count; k++)
		if (addr >= reserved[k].start && addr < reserved[k].end) return 1;
	return 0;
}

// "Maps" part of a library file by reading it into place; like mmap, bytes
// past the end of the file read as zero.
static void place(long fd, uint64_t at, uint64_t len, uint64_t off)
{
	uint64_t done = 0;
	while (done < len) {
		long n = __wasm_env_host_syscall(SYS_pread64, fd, (long)(at + done), (long)(len - done), (long)(off + done), 0, 0);
		if (n <= 0) break;
		done += (uint64_t)n;
	}
	__builtin_memset((void *)(uintptr_t)(at + done), 0, len - done);
}

// Handles the syscalls that involve translated libraries. Returns 1 if it
// did (the result is in x0).
static int library_syscall(long nr)
{
	switch (nr) {
	case SYS_openat: {
		long fd = __wasm_env_host_syscall(nr, (long)R[0], (long)R[1], (long)R[2], (long)R[3], 0, 0);
		if (fd >= 0 && fd < LIB_FDS) {
			long k = lib_for_path((const char *)(uintptr_t)R[1]);
			fd_lib[fd] = (uint16_t)(k + 1); // 0 (unknown) if not translated with the program
			pending_fd = fd + 1;
		}
		R[0] = (uint64_t)fd;
		return 1;
	}
	case SYS_close:
		if (R[0] < LIB_FDS) fd_lib[R[0]] = 0;
		if ((long)R[0] + 1 == pending_fd) pending_fd = 0;
		return 0;
	case SYS_mmap: {
		uint64_t addr = R[0], len = R[1], flags = R[3], off = R[5];
		long fd = (long)R[4];
		if ((flags & MAP_ANONYMOUS) && !(flags & MAP_FIXED) && R[2] == 0 && pending_fd
		    && reservation_count < RESERVATIONS) {
			// glibc's reservation for an aligned library (see pending_fd),
			// at the library's base plus its first segment's address, which
			// it passes as a hint (0 for most shared objects).
			long k = lib_of_fd(pending_fd - 1);
			if (k < 0)
				return 0;
			reserved[reservation_count].start = libs[k].base + addr;
			reserved[reservation_count++].end = libs[k].base + addr + len;
			R[0] = libs[k].base + addr;
			return 1;
		}
		if (!(flags & MAP_ANONYMOUS)) {
			long k = lib_of_fd(fd);
			if (k < 0)
				return 0;
			pending_fd = 0;
			// The first mapping of a library passes its lowest segment
			// address as a hint (0 for a shared object): map at base + hint.
			uint64_t at = (flags & MAP_FIXED) ? addr : libs[k].base + addr;
			place(fd, at, len, off);
			R[0] = at;
			return 1;
		}
		if ((flags & MAP_FIXED) && (flags & MAP_ANONYMOUS) && in_a_lib(addr)) {
			__builtin_memset((void *)(uintptr_t)addr, 0, len); // .bss
			R[0] = addr;
			return 1;
		}
		return 0;
	}
	case SYS_munmap:
		if (in_a_lib(R[0])) { // dlclose: the translation stays
			R[0] = 0;
			return 1;
		}
		return 0;
	}
	return 0;
}

// svc #0: the syscall number is in x8, arguments in x0–x5, the result goes
// to x0. arm64 Linux uses the same generic syscall table as wasm-env, so
// calls pass straight through.
// vfork for translated code. The runtime's vfork (docs/abi.md, "Processes")
// runs the child in the parent's memory until it execs or exits, then
// returns the child's pid from that execve/exit. Here the parent's guest
// registers are saved at the clone and restored then, and the lifted code
// resumes the parent after its clone (with the pid in x0), redirected by
// __wasm_env_svc_at's return value. The child may run on the parent's stack
// (glibc's vfork) or a new one (glibc's posix_spawn); either way the parent
// was waiting at its clone, as with a real CLONE_VFORK.
struct vfork_frame {
	uint64_t r[32], tp, resume;
	uint8_t f[4];
	__uint128_t v[32];
};
#define VFORK_DEPTH 4
static _Thread_local struct vfork_frame vfork_frames[VFORK_DEPTH];
static _Thread_local int vfork_depth;
static _Thread_local uint64_t svc_next;

enum { SYS_uname = 160, SYS_exit = 93, SYS_execve = 221, CLONE_VM = 0x100, CLONE_VFORK = 0x4000 };

static void vfork_clone(void)
{
	uint64_t flags = R[0], stack = R[1];
	if ((flags & (CLONE_VM | CLONE_VFORK)) != (CLONE_VM | CLONE_VFORK) || vfork_depth == VFORK_DEPTH) {
		// fork: not yet.
		R[0] = (uint64_t)-ENOSYS;
		return;
	}
	long r = __wasm_env_host_syscall(SYS_clone, (long)flags, 0, 0, 0, 0, 0);
	if (r < 0) {
		R[0] = (uint64_t)r;
		return;
	}
	struct vfork_frame *f = &vfork_frames[vfork_depth++];
	__builtin_memcpy(f->r, R, sizeof f->r);
	__builtin_memcpy(f->f, F, sizeof f->f);
	__builtin_memcpy(f->v, V, sizeof f->v);
	f->tp = TP;
	f->resume = svc_next;
	R[0] = 0; // the child
	if (stack)
		R[31] = stack;
}

// The vfork child exec'd or exited (the runtime returned its pid): continue
// as the parent, after its clone.
static void vfork_resume(long pid)
{
	struct vfork_frame *f = &vfork_frames[--vfork_depth];
	__builtin_memcpy(R, f->r, sizeof f->r);
	__builtin_memcpy(F, f->f, sizeof f->f);
	__builtin_memcpy(V, f->v, sizeof f->v);
	TP = f->tp;
	R[0] = (uint64_t)pid;
	svc_redirect = f->resume;
}

// Threads (docs/abi.md, "Threads"). The runtime runs each guest thread as
// its own instance of this module, sharing its memory, with its own
// thread-local guest state. clone(CLONE_THREAD) passes the runtime a frame
// holding the new thread's registers: the parent's, with x0 = 0, SP = the
// new stack and TPIDR_EL0 = the tls argument (CLONE_SETTLS), resuming after
// the clone. The runtime starts the thread at __wasm_env_thread_start with
// that frame and returns its tid to the parent once the thread has copied
// the frame.
enum { CLONE_SETTLS = 0x80000, CLONE_THREAD = 0x10000 };
#define WASM_ENV_THREAD_STARTED ((1L << 32) + 1)
static _Thread_local struct vfork_frame clone_frame;

static void thread_clone(void)
{
	uint64_t flags = R[0], stack = R[1], tls = R[3];
	struct vfork_frame *f = &clone_frame;
	__builtin_memcpy(f->r, R, sizeof f->r);
	__builtin_memcpy(f->f, F, sizeof f->f);
	__builtin_memcpy(f->v, V, sizeof f->v);
	f->r[0] = 0;
	if (stack)
		f->r[31] = stack;
	f->tp = (flags & CLONE_SETTLS) ? tls : TP;
	f->resume = svc_next;
	R[0] = (uint64_t)__wasm_env_host_syscall(SYS_clone, (long)flags, (long)stack, (long)R[2], (long)tls, (long)R[4],
	                                         (long)f);
}

__attribute__((__export_name__("__wasm_env_thread_start")))
void __wasm_env_thread_start(const struct vfork_frame *f)
{
	__wasm_env_set_state(&guest);
	__builtin_memcpy(R, f->r, sizeof f->r);
	__builtin_memcpy(F, f->f, sizeof f->f);
	__builtin_memcpy(V, f->v, sizeof f->v);
	TP = f->tp;
	uint64_t pc = f->resume;
	// The parent may go on (and reuse the frame) from here.
	__wasm_env_host_syscall(WASM_ENV_THREAD_STARTED, 0, 0, 0, 0, 0, 0);
	dispatch((long)pc);
}

// fork (clone without CLONE_VM; docs/abi.md, "Processes"). The runtime
// makes the child a copy of this process, memory and all, and starts it at
// __wasm_env_fork_start, which resumes after the clone with x0 = 0: the
// child's copy of this thread's guest state is this one's. The parent gets
// the child's pid.
static void fork_clone(void)
{
	uint64_t flags = R[0], stack = R[1];
	struct vfork_frame *f = &clone_frame;
	f->resume = svc_next;
	f->r[31] = stack ? stack : R[31];
	R[0] = (uint64_t)__wasm_env_host_syscall(SYS_clone, (long)flags, (long)stack, (long)R[2], (long)R[3], (long)R[4],
	                                         (long)f);
}

__attribute__((__export_name__("__wasm_env_fork_start")))
void __wasm_env_fork_start(const struct vfork_frame *f)
{
	__wasm_env_set_state(&guest);
	R[0] = 0;
	R[31] = f->r[31];
	dispatch((long)f->resume);
}



void __wasm_env_svc(void)
{
	long nr = (long)R[8];
	if (nr == SYS_rt_sigreturn) {
		sigreturn();
		return;
	}
	if (nr == SYS_clone) {
		if (R[0] & CLONE_THREAD)
			thread_clone();
		else if (!(R[0] & CLONE_VM))
			fork_clone();
		else
			vfork_clone();
		return;
	}
	if (library_syscall(nr)) return;
	if (vfork_depth && (nr == SYS_execve || nr == SYS_exit || nr == SYS_exit_group)) {
		long r = __wasm_env_host_syscall(nr == SYS_exit ? SYS_exit_group : nr, (long)R[0], (long)R[1],
		                                 (long)R[2], 0, 0, 0);
		if (r > 0)
			vfork_resume(r);
		else
			R[0] = (uint64_t)r; // a failed execve: the child goes on
		return;
	}
	// kill, tkill or tgkill with SIGABRT: the program is aborting.
	if (debug && ((nr == 129 && R[1] == 6) || (nr == 130 && R[1] == 6) || (nr == 131 && R[2] == 6)))
		print_recent(RECENT);
	uint64_t arg0 = R[0];
	R[0] = (uint64_t)__wasm_env_host_syscall(nr, (long)R[0], (long)R[1], (long)R[2], (long)R[3],
	                                         (long)R[4], (long)R[5]);
	if (nr == SYS_uname && R[0] == 0) {
		// The runtime reports its own machine (wasm64); a translated program
		// is an arm64 program, and code chooses paths and packages by it.
		// struct utsname: six 65-byte fields, machine the fifth.
		char *machine = (char *)(uintptr_t)arg0 + 4 * 65;
		__builtin_memset(machine, 0, 65);
		__builtin_memcpy(machine, "aarch64", 7);
	}
}

// Signal handlers (docs/abi.md, "Signals"). When a syscall returns with a
// signal to deliver (guest.signal, which the runtime fills in), the support
// code does what the arm64 kernel does: it pushes a struct rt_sigframe on
// the guest stack (or the alternate stack, for SA_ONSTACK), holding the
// siginfo and a ucontext with the interrupted state (registers, flags, the
// FP/SIMD record, the old signal mask), and enters the handler with x0 =
// the signal, x1 = &siginfo, x2 = &ucontext and x30 = the restorer
// (SA_RESTORER), which calls rt_sigreturn; that restores the state from
// the frame and resumes where the signal interrupted.
enum { SYS_rt_sigprocmask = 135, SA_ONSTACK = 0x08000000, SA_RESTORER = 0x04000000, SS_DISABLE = 2 };
#define FPSIMD_MAGIC 0x46508001u

struct sigframe {
	struct {
		int32_t signo, errno_, code, pad;
		uint8_t rest[112];
	} info;
	struct {
		uint64_t flags, link;
		uint64_t ss_sp;
		int32_t ss_flags, ss_pad;
		uint64_t ss_size;
		uint64_t sigmask;
		uint8_t unused[120];
		uint64_t pad; // mcontext is 16-byte aligned
		struct {
			uint64_t fault_address, regs[31], sp, pc, pstate;
			uint64_t pad;
			// __reserved[4096]: an fpsimd_context record, then a terminator.
			struct {
				uint32_t magic, size, fpsr, fpcr;
				__uint128_t vregs[32];
			} fpsimd;
			uint32_t end_magic, end_size;
			uint8_t reserved_rest[4096 - 528 - 8];
		} mcontext;
	} uc;
};
_Static_assert(__builtin_offsetof(struct sigframe, uc) == 128, "rt_sigframe");
_Static_assert(__builtin_offsetof(struct sigframe, uc.mcontext) == 128 + 176, "ucontext");
_Static_assert(__builtin_offsetof(struct sigframe, uc.mcontext.fpsimd) == 128 + 176 + 288, "sigcontext");
_Static_assert(sizeof(struct sigframe) == 128 + 176 + 288 + 4096, "rt_sigframe size");

// Enters the handler for guest.signal; `resume` is where the interrupted
// code goes on (after the syscall).
static void enter_handler(uint64_t resume)
{
	struct signal_delivery d = guest.signal;
	guest.signal.sig = 0;
	uint64_t sp = R[31];
	int on_alt = d.alt_size && sp > d.alt_sp && sp <= d.alt_sp + d.alt_size;
	if ((d.flags & SA_ONSTACK) && !(d.alt_flags & SS_DISABLE) && d.alt_size && !on_alt)
		sp = d.alt_sp + d.alt_size;
	sp = (sp - sizeof(struct sigframe)) & ~(uint64_t)15;
	struct sigframe *f = (struct sigframe *)(uintptr_t)sp;
	__builtin_memset(f, 0, sizeof *f);
	f->info.signo = (int32_t)d.sig; // si_code SI_USER (0)
	f->uc.ss_sp = d.alt_sp;
	f->uc.ss_flags = (int32_t)d.alt_flags;
	f->uc.ss_size = d.alt_size;
	f->uc.sigmask = d.old_mask;
	__builtin_memcpy(f->uc.mcontext.regs, R, sizeof f->uc.mcontext.regs);
	f->uc.mcontext.sp = R[31];
	f->uc.mcontext.pc = resume;
	f->uc.mcontext.pstate = (uint64_t)F[0] << 31 | (uint64_t)F[1] << 30 | (uint64_t)F[2] << 29 | (uint64_t)F[3] << 28;
	f->uc.mcontext.fpsimd.magic = FPSIMD_MAGIC;
	f->uc.mcontext.fpsimd.size = sizeof f->uc.mcontext.fpsimd;
	f->uc.mcontext.fpsimd.fpsr = guest.fpsr;
	__builtin_memcpy(f->uc.mcontext.fpsimd.vregs, V, sizeof f->uc.mcontext.fpsimd.vregs);
	R[0] = d.sig;
	R[1] = (uint64_t)(uintptr_t)&f->info;
	R[2] = (uint64_t)(uintptr_t)&f->uc;
	R[29] = sp;
	// Without SA_RESTORER the kernel returns through its vDSO trampoline (glibc
	// relies on it); here, through an address dispatch takes as rt_sigreturn.
	R[30] = (d.flags & SA_RESTORER) ? d.restorer : SIGRETURN_PC;
	R[31] = sp;
	svc_redirect = d.handler;
}

// rt_sigreturn: the handler returned (through the restorer) with SP at the
// frame. Restore what it saved, including the signal mask.
static void sigreturn(void)
{
	const struct sigframe *f = (const struct sigframe *)(uintptr_t)R[31];
	__wasm_env_host_syscall(SYS_rt_sigprocmask, 2 /* SIG_SETMASK */, (long)&f->uc.sigmask, 0, 8, 0, 0);
	__builtin_memcpy(R, f->uc.mcontext.regs, sizeof f->uc.mcontext.regs);
	R[31] = f->uc.mcontext.sp;
	uint64_t pstate = f->uc.mcontext.pstate;
	F[0] = pstate >> 31 & 1;
	F[1] = pstate >> 30 & 1;
	F[2] = pstate >> 29 & 1;
	F[3] = pstate >> 28 & 1;
	if (f->uc.mcontext.fpsimd.magic == FPSIMD_MAGIC) {
		guest.fpsr = f->uc.mcontext.fpsimd.fpsr;
		__builtin_memcpy(V, f->uc.mcontext.fpsimd.vregs, sizeof V);
	}
	svc_redirect = f->uc.mcontext.pc;
}

// The lifted code's SVC: `next` is the address after it. Returns where to
// continue instead, or 0 to go on (see vfork_clone).
int64_t __wasm_env_svc_at(int64_t next)
{
	svc_next = (uint64_t)next;
	svc_redirect = 0;
	__wasm_env_svc();
	check_code_epoch();
	if (guest.signal.sig)
		enter_handler(svc_redirect ? svc_redirect : svc_next);
	return (int64_t)svc_redirect;
}

// The guest's stack. The wasm-env runtime puts the initial process stack
// just below the module's own stack pointer, which the support code keeps
// using, so the guest gets a separate region. 8 MB, like Linux.
#define GUEST_STACK (8u << 20)
static uint8_t guest_stack[GUEST_STACK] __attribute__((aligned(16)));

enum { AT_NULL = 0, AT_PHDR = 3, AT_PHENT = 4, AT_PHNUM = 5, AT_BASE = 7, AT_ENTRY = 9, AT_HWCAP = 16, AT_HWCAP2 = 26 };

// The CPU features translated code may use: exactly what the translator
// implements (FP, ASIMD, CRC32, LSE atomics, LRCPC), so libraries choose
// those paths and not, say, SVE, AES or half-precision arithmetic. Programs
// such as numpy check their build baseline against these at startup.
enum { HWCAP_FP = 1 << 0, HWCAP_ASIMD = 1 << 1, HWCAP_CRC32 = 1 << 7, HWCAP_ATOMICS = 1 << 8, HWCAP_LRCPC = 1 << 15 };
#define GUEST_HWCAP (HWCAP_FP | HWCAP_ASIMD | HWCAP_CRC32 | HWCAP_ATOMICS | HWCAP_LRCPC)

// Copies argc, argv, envp and auxv from the runtime's initial stack to the
// guest stack, adding the ELF entries the kernel would (AT_PHDR for TLS
// setup and the dynamic linker, AT_ENTRY, and AT_BASE when a dynamic linker
// runs). The strings stay where they are.
// When the program's libraries are modules of their own, the runtime loads
// the dynamic linker (as the kernel would) and adds its load address
// (AT_BASE) and where to start (its entry) to the auxiliary vector.
enum { WASM_ENV_AT_START = 0x7700 };
static uint64_t start_pc;

static uint64_t guest_initial_stack(const uint64_t *sp)
{
	uint64_t argc = sp[0];
	const uint64_t *p = sp + 1 + argc + 1;
	while (*p) p++;                  // envp
	for (const uint64_t *e = sp + 1 + argc + 1; *e; e++) {
		const char *s = (const char *)(uintptr_t)*e, *want = "WASM_ENV_DEBUG=1";
		while (*want && *s == *want) s++, want++;
		if (!*want && !*s) debug = 1;
	}
	const uint64_t *auxv = p + 1;
	const uint64_t *q = auxv;
	while (q[0] != AT_NULL) q += 2;
	uint64_t words = (uint64_t)(q - sp);  // argc through the last aux pair
	uint64_t total = words + 5 * 2 + 2;   // plus up to five new pairs and AT_NULL

	uint64_t *out = (uint64_t *)(guest_stack + GUEST_STACK) - total;
	out = (uint64_t *)((uintptr_t)out & ~(uintptr_t)15);
	for (uint64_t i = 0; i < words; i++)
		out[i] = sp[i];
	uint64_t *a = out + words;
	*a++ = AT_PHDR;  *a++ = guest_phdr;
	*a++ = AT_PHENT; *a++ = 56;
	*a++ = AT_PHNUM; *a++ = guest_phnum;
	*a++ = AT_ENTRY; *a++ = guest_app_entry;
	if (guest_interp_base) {
		*a++ = AT_BASE; *a++ = guest_interp_base;
	}
	*a++ = AT_NULL;  *a++ = 0;
	// Report the features the translation supports (the runtime says none),
	// and note where the runtime says to start (see WASM_ENV_AT_START).
	for (uint64_t *p = out + (auxv - sp); p[0] != AT_NULL; p += 2) {
		if (p[0] == AT_HWCAP) p[1] = GUEST_HWCAP;
		if (p[0] == AT_HWCAP2) p[1] = 0;
		if (p[0] == WASM_ENV_AT_START) start_pc = p[1];
	}
	return (uint64_t)out;
}

__attribute__((__export_name__("_start")))
void _start(uint64_t sp)
{
	__wasm_env_set_state(&guest);
	// Map the guest image at its original addresses. The translator places
	// segments there as the module's own data segments (data is null), and
	// memory starts zeroed; otherwise copy them.
	for (uint64_t k = 0; k < guest_segment_count; k++) {
		const struct seg *s = &guest_segments[k];
		if (!s->data)
			continue;
		uint8_t *dst = (uint8_t *)(uintptr_t)s->vaddr;
		__builtin_memcpy(dst, s->data, s->filesz);
		__builtin_memset(dst + s->filesz, 0, s->memsz - s->filesz);
	}
	// Linux starts arm64 processes with every register zero except SP.
	R[31] = guest_initial_stack((const uint64_t *)(uintptr_t)sp);
	for (uint64_t k = 0; k < guest_lib_count; k++) {
		libs[k].base = guest_libs[k].base;
		libs[k].end = guest_libs[k].end;
	}
	lib_count = guest_lib_count;
	dispatch(start_pc ? (long)start_pc : (long)guest_entry);
}

// This thread's guest registers, for tests and debugging.
__attribute__((__export_name__("__wasm_env_registers")))
uint64_t *__wasm_env_registers(void)
{
	return R;
}
