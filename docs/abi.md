# wasm-env native ABI (`wasm64-linux-musl`), version 0

This document defines what a wasm-env program is: the contract between a WebAssembly module and the wasm-env runtime. Recompiled software targets it through our toolchain. Translated binaries (see PLAN.md) will target it too. Status: draft (M0, extended in M1 with processes); everything may change until M3.

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
- Above the break's region, the runtime manages the address space for `mmap`, in 4 KB pages, growing the WebAssembly memory as needed. Anonymous mappings come back zero-filled. `munmap` returns pages for reuse.
- The program break starts at `__heap_base` and `brk` moves it within a 64 MB region (zero-filling what it exposes); a request beyond that returns the current break, which is how Linux reports failure, and allocators fall back to `mmap`. The `mmap` region begins above the break's.
- Page protections (`mprotect`) are accepted and ignored: WebAssembly can't enforce them inside linear memory. `PROT_EXEC` mappings are meaningless, since code can't be generated at runtime (see non-goals in PLAN.md).
- File-backed `mmap` is copied in on `mmap` (and, for `MAP_SHARED` writable mappings, written back on `msync`/`munmap`). Version 0 supports read-only file mappings only.

## Threads and thread-local storage

Version 0 is single-threaded. `clone` with `CLONE_THREAD` returns `-ENOSYS`, so `pthread_create` fails cleanly. `futex` behaves as it does for a process with one thread: a wake finds no waiters, and a wait returns `-EAGAIN` if the word changed, otherwise sleeps out its timeout (`-ETIMEDOUT`) or returns `-EINTR` as a spurious wakeup. (Separate processes do run concurrently; see "Processes".)

musl's thread pointer, which it uses for `errno` and its own bookkeeping, is an ordinary global variable in libc, set by `__set_thread_area`. There is no ELF TLS segment. C `_Thread_local` variables compile to ordinary globals, which is correct with one thread.

## `setjmp` and `longjmp`

The compiler rewrites `setjmp`/`longjmp` using WebAssembly exception handling (`-mllvm -wasm-enable-sjlj`): `longjmp` throws the C tag `__c_longjmp`, and functions that call `setjmp` catch it and check whether it's theirs. libc supplies the three helpers the compiler calls (`__wasm_setjmp`, `__wasm_setjmp_test`, `__wasm_longjmp`). Everything linked into a program must be compiled with the same flags.

## Processes

Processes are created with **`vfork` + `execve`**, which covers `posix_spawn`, `system`, `popen` and shells. There is no `fork` yet (see the end of this section).

**vfork.** `clone(CLONE_VM | CLONE_VFORK | SIGCHLD, 0)` (no new stack) enters a *vfork child*:

1. The runtime allocates the child's pid and saves the parent's process state (descriptor table, working directory, umask, signal dispositions). The child continues **in the parent's memory, on the parent's call stack**, with its own copy of that state, exactly as a real vfork child shares its parent's memory.
2. When the child calls `execve` and it succeeds, the runtime starts the new program as the child's pid, restores the parent's state, and **returns the child's pid** (instead of not returning).
3. When the child calls `exit_group`, the runtime records the child's exit status, restores the parent's state, and **returns the child's pid**.
4. In both cases libc then `longjmp`s back to the `vfork` call site, where `vfork()` returns the pid to the parent.

`vfork` returns twice, so libc defines it as a macro around `setjmp` (see `toolchain/musl-overlay/src/process/wasm64/vfork.c`); the child must not return from the function that called `vfork`, the usual vfork rule. `clone` with any other flags returns `-ENOSYS`.

Translated (arm64) programs use the same protocol through the translator's support code, so their own libc's `vfork`, `posix_spawn` and `system` work: at the guest's `clone(CLONE_VM | CLONE_VFORK, stack)` the support code saves the guest registers and the address after the `svc`, and the child continues (on `stack` if one is given, as glibc's `posix_spawn` does). When the runtime returns the child's pid from its `execve` or `exit`, the support code restores the registers, puts the pid in x0 and resumes the parent after its `clone`.

**execve** accepts WebAssembly programs, `#!` scripts (up to four levels of interpreter) and arm64 ELF binaries, which the runtime translates on first use (with their dynamic linker and libraries from the environment) and caches. Other files fail with `-ENOEXEC`. Outside a vfork child, a successful `execve` replaces the running program; the pid, descriptors (minus close-on-exec ones), working directory, umask and ignored signals carry over.

**Process model.** Each process runs on its own host thread, all inside the environment's one host process. `wait4` supports the usual targets (-1, a pid, 0, -pgid) and `WNOHANG`, and reports standard Linux wait statuses. Pipes are in-runtime buffers of 64 KB; writing to a pipe without readers raises `SIGPIPE`.

**Signals (version 0).** `kill` and `tgkill` queue a signal; it's acted on at the target's next syscall, and blocking calls (`read` on a pipe, `wait4`, `nanosleep`) wake up with `-EINTR`. Default actions apply (terminate, or ignore for SIGCHLD, SIGCONT, SIGURG and SIGWINCH), and `SIG_IGN` is honored. **Signal handlers are recorded by `rt_sigaction` but never run**: a signal with a handler is ignored. Running handlers is future work.

**Devices.** The runtime provides `/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr` and `/dev/fd/N`, whatever the root directory contains.

**Networking.** Sockets are the host's: a program reaches the network, and binds ports, as the host would. Supported: `AF_INET` and `AF_INET6` sockets, stream and datagram (`SOCK_NONBLOCK` and `SOCK_CLOEXEC` too); `connect` (a non-blocking one reports `-EINPROGRESS`, then its result through `SO_ERROR`), `bind`, `listen`, `accept`/`accept4`, `getsockname`, `getpeername`, `sendto`, `recvfrom`, `sendmsg` and `recvmsg` (no control messages), `sendmmsg`, `shutdown`, `read`/`write` on sockets, and `setsockopt`/`getsockopt` for the common options (others are accepted and ignored). Blocking follows the descriptor's `O_NONBLOCK` (`fcntl` or `ioctl(FIONBIO)`). `ppoll` and `pselect6` work across sockets, pipes and files (files are always ready). Other address families (`AF_UNIX`, `AF_NETLINK`) return `-EAFNOSUPPORT`; glibc copes (no nscd; `getaddrinfo` assumes both IP families). Names resolve as the guest's libc does, from the root's `/etc/hosts` and `/etc/resolv.conf`; if the root's `/etc/resolv.conf` is missing or empty, as in a fresh image, the runtime provides the host's (as Docker does for containers). Writing to a broken connection returns `-EPIPE` without raising `SIGPIPE`.

**fork**, for programs that fork without exec, remains open. The options:

| Option | How it works | Trade-off |
|---|---|---|
| Asyncify-style unwinding | The compiler instruments code so the call stack can be saved to memory and rebuilt in a copy of the process | Works on any runtime; code grows and slows (roughly 10–50%) |
| Stack switching | Use the WebAssembly stack-switching proposal to capture the running stack | Cheap, but the proposal isn't finished and wasmtime support is experimental |
| Runtime-level snapshot | wasmtime copies the instance, including its native stack | No compiler changes; needs deep wasmtime changes |

## Toolchain

- Target: `clang --target=wasm64-unknown-unknown`, wrapped by `wasm64-linux-musl-cc`, which sets the sysroot and flags and defines `__linux__` and `__unix__` (but not `__gnu_linux__`, since this is musl). clang has no `wasm64-*-linux` target, so the wrapper stands in for one.
- `long double` is 128-bit IEEE quad, as on arm64 Linux, emulated in software (compiler-rt builtins).
- Linking is static: `crt1.o`, the program, `libc.a` and compiler-rt builtins, linked by `wasm-ld`. Dynamic linking is out of scope.

## Planned versions

- **Version 1: dynamic linking** (PLAN.md, M3). Several modules in one process, sharing one memory and one function table, loaded and linked by the runtime at run time, including `dlopen`. Needed so translated programs can load shared libraries such as Python extension modules. Translated modules use glibc's symbol versions.
- **Version 2: threads** (PLAN.md, M6). Shared memory, atomics and `clone(CLONE_THREAD)`, with real thread-local storage. Needed for PyTorch.

## wasm-env's own calls

Syscall numbers from 2^32 up are wasm-env's, not Linux's; unknown ones return `-ENOSYS` like any other.

| Number | Call | Arguments |
|---|---|---|
| 2^32 | `missed_target` | (address, object name, offset in the object). Made by a translated program's support code when it jumps to code the translation missed. The runtime records the address, and the next translation of that program includes it. |

## Versioning

The module declares the ABI version it was built for in a custom section named `wasm-env.abi`, containing the version number as a LEB128 integer. Version 0 is this document. The runtime refuses versions it doesn't know.
