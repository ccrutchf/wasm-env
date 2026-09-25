// Support code linked into every translated program.
//
// The lifted code (translator/src/lift) keeps guest state in globals and
// calls into here for program startup, system calls and fatal errors. This
// file is compiled for wasm64 without libc; it only uses the linux.syscall
// import (docs/abi.md).
#include <stdint.h>

long __wasm_env_host_syscall(long, long, long, long, long, long, long)
	__attribute__((__import_module__("linux"), __import_name__("syscall")));

// Guest state, defined by the lifted code.
extern uint64_t R[32]; // x0–x30, SP
extern uint64_t TP;    // TPIDR_EL0

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
typedef void (*block_fn)(long);

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

void __wasm_env_bad_target(uint64_t pc)
{
	for (uint64_t k = 0; k < guest_object_count; k++) {
		const struct lib *o = &guest_objects[k];
		if (pc >= o->base && pc < o->end)
			__wasm_env_host_syscall(WASM_ENV_MISSED_TARGET, (long)pc, (long)o->path, (long)(pc - o->base), 0, 0, 0);
	}
	die("jump to an address that wasn't translated", pc);
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
// A direct-mapped cache of region lookups, tagged with region number + 1 so
// the zeroed initial entries match nothing. Processes are single-threaded.
#define DISPATCH_CACHE 4096
static struct {
	uint64_t tag;
	block_fn fn;
} dispatch_cache[DISPATCH_CACHE];

// Find `pc`'s region function and tail-call it (a WebAssembly return_call,
// so the host stack doesn't grow): from the cache, else by binary search of
// the region table. An address in no region was never translated.
void dispatch(long pc)
{
	uint64_t target = (uint64_t)pc;
	wasm_env_recent[wasm_env_recent_count++ % RECENT] = target;
	uint64_t region = target >> REGION_SHIFT;
	uint64_t slot = (region ^ (region >> 12)) % DISPATCH_CACHE;
	if (dispatch_cache[slot].tag == region + 1) {
		block_fn f = dispatch_cache[slot].fn;
		__attribute__((musttail)) return f(pc); // the region function enters at pc
	}
	uint64_t start = region << REGION_SHIFT;
	uint64_t lo = 0, hi = dispatch_count;
	while (lo < hi) {
		uint64_t mid = lo + (hi - lo) / 2;
		uint64_t a = dispatch_addrs[mid];
		if (a == start) {
			block_fn f = (block_fn)dispatch_fns[mid];
			dispatch_cache[slot].tag = region + 1;
			dispatch_cache[slot].fn = f;
			__attribute__((musttail)) return f(pc);
		}
		if (a < start)
			lo = mid + 1;
		else
			hi = mid;
	}
	__wasm_env_bad_target(target);
}

// Shared libraries the translated dynamic linker loads at run time. Each
// was translated with the program at a fixed base, so its code already
// exists as translated blocks there. When the dynamic linker opens one and
// maps it, place the mapping at that base; it then relocates the library as
// usual, and every address it computes matches the translation.
extern const struct lib guest_libs[];
extern const uint64_t guest_lib_count;

enum { SYS_openat = 56, SYS_close = 57, SYS_pread64 = 67, SYS_munmap = 215, SYS_mmap = 222 };
enum { MAP_FIXED = 0x10, MAP_ANONYMOUS = 0x20 };

// For each file descriptor, 1 + the library it has open (0: none).
#define LIB_FDS 1024
static uint16_t fd_lib[LIB_FDS];

// The library most recently opened and not yet mapped (1 + index, or 0).
// glibc maps a library whose segments are aligned beyond the page size by
// first reserving an anonymous, inaccessible region, then mapping the file
// at an aligned address inside it; that reservation must be the library's
// prelinked base.
static uint64_t pending_lib;

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

// The library a path names: the exact path the translator found, or else
// the same file name (the dynamic linker may reach it through another
// directory, such as one from LD_LIBRARY_PATH).
static long lib_for_path(const char *path)
{
	for (uint64_t k = 0; k < guest_lib_count; k++)
		if (same(path, guest_libs[k].path)) return (long)k;
	for (uint64_t k = 0; k < guest_lib_count; k++)
		if (same(base_name(path), base_name(guest_libs[k].path))) return (long)k;
	return -1;
}

static int in_a_lib(uint64_t addr)
{
	for (uint64_t k = 0; k < guest_lib_count; k++)
		if (addr >= guest_libs[k].base && addr < ((guest_libs[k].end + 4095) & ~(uint64_t)4095)) return 1;
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
	if (!guest_lib_count) return 0;
	switch (nr) {
	case SYS_openat: {
		long fd = __wasm_env_host_syscall(nr, (long)R[0], (long)R[1], (long)R[2], (long)R[3], 0, 0);
		if (fd >= 0 && fd < LIB_FDS) {
			fd_lib[fd] = (uint16_t)(lib_for_path((const char *)(uintptr_t)R[1]) + 1);
			pending_lib = fd_lib[fd];
		}
		R[0] = (uint64_t)fd;
		return 1;
	}
	case SYS_close:
		if (R[0] < LIB_FDS) fd_lib[R[0]] = 0;
		return 0;
	case SYS_mmap: {
		uint64_t addr = R[0], len = R[1], flags = R[3], off = R[5];
		long fd = (long)R[4];
		if ((flags & MAP_ANONYMOUS) && !(flags & MAP_FIXED) && addr == 0 && R[2] == 0 && pending_lib
		    && reservation_count < RESERVATIONS) {
			// glibc's reservation for an aligned library (see pending_lib).
			uint64_t base = guest_libs[pending_lib - 1].base;
			reserved[reservation_count].start = base;
			reserved[reservation_count++].end = base + len;
			R[0] = base;
			return 1;
		}
		if (!(flags & MAP_ANONYMOUS) && fd >= 0 && fd < LIB_FDS && fd_lib[fd]) {
			pending_lib = 0;
			// The first mapping of a library passes its lowest segment
			// address as a hint (0 for a shared object): map at base + hint.
			uint64_t at = (flags & MAP_FIXED) ? addr : guest_libs[fd_lib[fd] - 1].base + addr;
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
void __wasm_env_svc(void)
{
	long nr = (long)R[8];
	if (nr == SYS_clone) {
		// A translated process can't vfork/fork yet: the runtime's vfork
		// protocol expects wasm-env's libc, not the guest's.
		R[0] = (uint64_t)-ENOSYS;
		return;
	}
	if (library_syscall(nr)) return;
	// kill, tkill or tgkill with SIGABRT: the program is aborting.
	if (debug && ((nr == 129 && R[1] == 6) || (nr == 130 && R[1] == 6) || (nr == 131 && R[2] == 6)))
		print_recent(RECENT);
	R[0] = (uint64_t)__wasm_env_host_syscall(nr, (long)R[0], (long)R[1], (long)R[2], (long)R[3],
	                                         (long)R[4], (long)R[5]);
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
	// Report the features the translation supports (the runtime says none).
	for (uint64_t *p = out + (auxv - sp); p[0] != AT_NULL; p += 2) {
		if (p[0] == AT_HWCAP) p[1] = GUEST_HWCAP;
		if (p[0] == AT_HWCAP2) p[1] = 0;
	}
	return (uint64_t)out;
}

__attribute__((__export_name__("_start")))
void _start(uint64_t sp)
{
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
	dispatch(guest_entry);
}
