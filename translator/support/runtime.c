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

// The block table, defined by the lifted data module: sorted guest block
// addresses and their translated functions. `dispatch` maps a guest address
// to its function; the lifted code routes every branch, call and return
// through it, so blocks never reference each other directly (which lets them
// be split across compilation units).
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

_Noreturn static void die(const char *what, uint64_t pc)
{
	put("wasm-env: translated program: ", 30);
	put(what, len(what));
	put(" at ", 4);
	put_hex(pc);
	put("\n", 1);
	__builtin_trap();
}

void __wasm_env_fatal(const char *why, uint64_t pc)
{
	die(why, pc);
}

void __wasm_env_bad_target(uint64_t pc)
{
	die("jump to an address that wasn't translated", pc);
}

// Binary search the block table for `pc` and tail-call its function (a
// WebAssembly return_call, so the host stack doesn't grow). An address with
// no block was never translated.
void dispatch(long pc)
{
	uint64_t target = (uint64_t)pc;
	uint64_t lo = 0, hi = dispatch_count;
	while (lo < hi) {
		uint64_t mid = lo + (hi - lo) / 2;
		uint64_t a = dispatch_addrs[mid];
		if (a == target) {
			block_fn f = (block_fn)dispatch_fns[mid];
			__attribute__((musttail)) return f(0);
		}
		if (a < target)
			lo = mid + 1;
		else
			hi = mid;
	}
	__wasm_env_bad_target(target);
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
	R[0] = (uint64_t)__wasm_env_host_syscall(nr, (long)R[0], (long)R[1], (long)R[2], (long)R[3],
	                                         (long)R[4], (long)R[5]);
}

// The guest's stack. The wasm-env runtime puts the initial process stack
// just below the module's own stack pointer, which the support code keeps
// using, so the guest gets a separate region. 8 MB, like Linux.
#define GUEST_STACK (8u << 20)
static uint8_t guest_stack[GUEST_STACK] __attribute__((aligned(16)));

enum { AT_NULL = 0, AT_PHDR = 3, AT_PHENT = 4, AT_PHNUM = 5, AT_BASE = 7, AT_ENTRY = 9 };

// Copies argc, argv, envp and auxv from the runtime's initial stack to the
// guest stack, adding the ELF entries the kernel would (AT_PHDR for TLS
// setup and the dynamic linker, AT_ENTRY, and AT_BASE when a dynamic linker
// runs). The strings stay where they are.
static uint64_t guest_initial_stack(const uint64_t *sp)
{
	uint64_t argc = sp[0];
	const uint64_t *p = sp + 1 + argc + 1;
	while (*p) p++;                  // envp
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
	return (uint64_t)out;
}

__attribute__((__export_name__("_start")))
void _start(uint64_t sp)
{
	// Map the guest image at its original addresses.
	for (uint64_t k = 0; k < guest_segment_count; k++) {
		const struct seg *s = &guest_segments[k];
		uint8_t *dst = (uint8_t *)(uintptr_t)s->vaddr;
		__builtin_memcpy(dst, s->data, s->filesz);
		__builtin_memset(dst + s->filesz, 0, s->memsz - s->filesz);
	}
	// Linux starts arm64 processes with every register zero except SP.
	R[31] = guest_initial_stack((const uint64_t *)(uintptr_t)sp);
	dispatch(guest_entry);
}
