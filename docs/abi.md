# wasm-env native ABI (`wasm64-linux-musl`), version 0

This document defines what a wasm-env program is: the contract between a WebAssembly module and the wasm-env runtime. Recompiled software targets it through our toolchain. Translated binaries (see PLAN.md) will target it too. Status: draft for M0; everything may change until M2.

The guiding rule: **be Linux**. Where Linux already defines something (syscall numbers, struct layouts, errno values, the initial process stack), we use the Linux definition, specifically the one for **arm64**, which uses the modern generic syscall table and 64-bit struct layouts. A port of existing software should see nothing unusual except the CPU.

## Module shape

A wasm-env program is a single core WebAssembly module (not a component).

| Item | Kind | Requirement |
|---|---|---|
| `memory` | exported memory | memory64 (64-bit addresses), the program's only memory |
| `_start` | exported function `(i64) -> ()` | Entry point; takes a pointer to the initial process stack (see below) |
| `__stack_pointer` | exported mutable `i64` global | The shadow-stack pointer (LLVM's C stack in linear memory) |
| `__heap_base` | exported `i64` global | First address after static data and the stack; the runtime's memory map starts here |
| `linux.syscall` | imported function `(i64 × 7) -> i64` | The only way to reach the system (see below) |

A module may import nothing else. The runtime rejects modules that don't match.

### WebAssembly features

Programs may use: memory64, bulk memory, mutable globals, sign extension, non-trapping float-to-int, multi-value, reference types, SIMD (128-bit), exception handling (the standard `exnref` form, used for `setjmp`/`longjmp` and C++ exceptions) and tail calls.

Not allowed in version 0: threads, shared memory, multiple memories.

## System calls

```
(import "linux" "syscall" (func (param $nr i64) (param i64 i64 i64 i64 i64 i64) (result i64)))
```

- `$nr` is a Linux syscall number from the **generic table** (`include/uapi/asm-generic/unistd.h`, as used by arm64). For example, `openat` is 56, `read` is 63, `write` is 64, `exit_group` is 94. Legacy calls that only exist on older architectures (`open`, `stat`, `fork`) don't exist here, just as on arm64.
- Up to six arguments, each an `i64`. Unused arguments are ignored. Pointers are addresses in the program's memory.
- The result is the raw kernel return value: a non-negative result on success, or `-errno` (−4095 to −1) on failure. Errno values are Linux's generic values. libc turns this into `errno`.
- Structures passed through pointers use arm64 Linux layouts (LP64: `long` and pointers are 8 bytes; `struct stat` is the generic 128-byte layout).
- A syscall the runtime doesn't implement returns `-ENOSYS` (−38). It never traps.
- The runtime validates every pointer and length against the program's memory; a bad pointer returns `-EFAULT`.

This mirrors the arm64 `svc #0` convention, so translated arm64 binaries can map their syscalls onto this import one-to-one, and x86-64 translation only needs a number-mapping table.

## Process startup

Linux starts a process with argc, argv, the environment and the auxiliary vector laid out at the top of its stack. wasm-env does the same, in the program's memory:

1. The runtime instantiates the module and reads `__stack_pointer`.
2. It writes the initial process stack just below that address, 16-byte aligned, in exactly the Linux layout:

   ```
   sp →  argc
         argv[0] … argv[argc-1], NULL
         envp[0] … envp[n-1], NULL
         auxv: (type, value) pairs …, (AT_NULL, 0)
         … padding, 16 random bytes (AT_RANDOM), argument and environment strings
   ```

   Every slot is 8 bytes.
3. It sets `__stack_pointer` to `sp` (so the C stack continues below the block) and calls `_start(sp)`.

`_start` is libc's (`crt1.o`), and it hands `sp` to `__libc_start_main` exactly as on Linux.

Auxiliary vector entries provided: `AT_PAGESZ` (4096), `AT_RANDOM`, `AT_UID`, `AT_EUID`, `AT_GID`, `AT_EGID`, `AT_SECURE` (0), `AT_HWCAP` (0), `AT_EXECFN`. There is no `AT_PHDR`, because a WebAssembly module has no ELF program headers.

The program exits by calling `exit_group`. If `_start` returns, the runtime treats it as `exit_group(0)`. A trap (for example, an out-of-bounds access below the masking limit) terminates the process as if by `SIGSEGV`: exit status 139 in the shell.

## Memory

- The program's memory is **masked memory64** (see PLAN.md, "Masked addressing"): up to 2^38 bytes (256 GB). Addresses above that wrap; out-of-bounds addresses below it trap.
- Static data and the C stack sit below `__heap_base`, laid out by the linker. The default stack is 8 MB, like Linux.
- Above `__heap_base`, the runtime manages the address space for `mmap`, in 4 KB pages, growing the WebAssembly memory as needed. Anonymous mappings come back zero-filled. `munmap` returns pages for reuse.
- `brk` never grows: it always returns the current break, which is how Linux reports failure. musl's allocator then uses `mmap`, which is its normal fallback.
- Page protections (`mprotect`) are accepted and ignored: WebAssembly can't enforce them inside linear memory. `PROT_EXEC` mappings are meaningless, since code can't be generated at runtime (see non-goals in PLAN.md).
- File-backed `mmap` is copied in on `mmap` (and, for `MAP_SHARED` writable mappings, written back on `msync`/`munmap`). Version 0 supports read-only file mappings only.

## Threads and thread-local storage

Version 0 is single-threaded. `clone` with `CLONE_THREAD` returns `-ENOSYS`, so `pthread_create` fails cleanly.

musl's thread pointer, which it uses for `errno` and its own bookkeeping, is an ordinary global variable in libc, set by `__set_thread_area`. There is no ELF TLS segment. C `_Thread_local` variables compile to ordinary globals, which is correct with one thread.

## `setjmp` and `longjmp`

The compiler rewrites `setjmp`/`longjmp` using WebAssembly exception handling (`-mllvm -wasm-enable-sjlj`): `longjmp` throws the C tag `__c_longjmp`, and functions that call `setjmp` catch it and check whether it's theirs. libc supplies the three helpers the compiler calls (`__wasm_setjmp`, `__wasm_setjmp_test`, `__wasm_longjmp`). Everything linked into a program must be compiled with the same flags.

## Processes: `fork` and `exec` (decided in M1)

Version 0 has no `fork`; `clone` without `CLONE_THREAD` (and `vfork`) returns `-ENOSYS`. `execve` is implemented by the runtime replacing the current module with a new one.

M1 must pick a `fork` strategy. The ABI above doesn't rule any of them out:

| Option | How it works | Trade-off |
|---|---|---|
| Asyncify-style unwinding | The compiler instruments code so the call stack can be saved to memory and rebuilt in a copy of the process | Works on any runtime; code grows and slows (roughly 10–50%) |
| Stack switching | Use the WebAssembly stack-switching proposal to capture the running stack | Cheap, but the proposal isn't finished and wasmtime support is experimental |
| Runtime-level snapshot | wasmtime copies the instance, including its native stack | No compiler changes; needs deep wasmtime changes |
| `posix_spawn`-first | Implement `vfork`+`exec` and `posix_spawn` without real `fork` | Covers shells and most tools; programs that fork without exec still fail |

## Toolchain

- Target: `clang --target=wasm64-unknown-unknown`, wrapped by `wasm64-linux-musl-cc`, which sets the sysroot and flags and defines `__linux__` and `__unix__` (but not `__gnu_linux__`, since this is musl). clang has no `wasm64-*-linux` target, so the wrapper stands in for one.
- `long double` is 128-bit IEEE quad, as on arm64 Linux, emulated in software (compiler-rt builtins).
- Linking is static: `crt1.o`, the program, `libc.a` and compiler-rt builtins, linked by `wasm-ld`. Dynamic linking is out of scope.

## Planned versions

- **Version 1: dynamic linking** (PLAN.md, M3). Several modules in one process, sharing one memory and one function table, loaded and linked by the runtime at run time, including `dlopen`. Needed so translated programs can load shared libraries such as Python extension modules. Translated modules use glibc's symbol versions.
- **Version 2: threads** (PLAN.md, M6). Shared memory, atomics and `clone(CLONE_THREAD)`, with real thread-local storage. Needed for PyTorch.

## Versioning

The module declares the ABI version it was built for in a custom section named `wasm-env.abi`, containing the version number as a LEB128 integer. Version 0 is this document. The runtime refuses versions it doesn't know.
