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

Not allowed in version 0: threads, shared memory, multiple memories. The runtime's arm64 translator produces modules of a threaded shape instead (see "Threads").

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
- Event loops (libuv's, Node.js's): `eventfd2`, `epoll_create1`, `epoll_ctl` and `epoll_pwait`/`epoll_pwait2` (level-triggered: `EPOLLET` behaves like it, which suits loops that read until `EAGAIN`; `EPOLLONESHOT` is honoured), and `socketpair` for `AF_UNIX` (host socket pairs).

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
- Above the break's region, the runtime manages the address space for `mmap`, in 4 KB pages, growing the WebAssembly memory as needed. Anonymous mappings come back zero-filled. `munmap` returns pages for reuse. An address hint is honoured, as on Linux, when that range is free: in the `mmap` region below its top, or in the break's region above the break (LuaJIT asks for code memory near its VM, within reach of direct branches); `brk` then can't grow into such a mapping.
- The program break starts at `__heap_base` and `brk` moves it within a 64 MB region (zero-filling what it exposes); a request beyond that returns the current break, which is how Linux reports failure, and allocators fall back to `mmap`. The `mmap` region begins above the break's.
- Page protections (`mprotect`) aren't enforced: WebAssembly can't, inside linear memory. `PROT_EXEC` marks where a program puts code it generates (see "Code generated at run time").
- File-backed `mmap` is copied in on `mmap` (and, for `MAP_SHARED` writable mappings, written back on `msync`/`munmap`). Version 0 supports read-only file mappings only.

## Threads and thread-local storage

Programs built with the wasm-env toolchain (version 0) are single-threaded. `clone` with `CLONE_THREAD` returns `-ENOSYS`, so `pthread_create` fails cleanly. musl's thread pointer, which it uses for `errno` and its own bookkeeping, is an ordinary global variable in libc, set by `__set_thread_area`. There is no ELF TLS segment. C `_Thread_local` variables compile to ordinary globals, which is correct with one thread.

**Threads.** Translated (arm64) programs can have threads. Their modules have a different shape:

| Item | Kind | Requirement |
|---|---|---|
| `env.memory` | imported memory | shared, memory64, with a maximum (the runtime creates it) |
| `__wasm_init_tls` | exported function `(i64) -> ()` | Points this instance's thread-local block at the address given, and initializes it |
| `__tls_size`, `__tls_align` | exported `i64` globals | The thread-local block's size and alignment |
| `__wasm_env_thread_start` | exported function `(i64) -> ()` | Starts a new thread from a clone frame (below) |
| `wasm-env.image` | custom section | The guest image: pages the runtime maps into memory copy-on-write before instantiating (translator/src/link.rs, `add_image`) |

plus `_start`, `__stack_pointer`, `__heap_base` and `linux.syscall` as above. wasm-ld builds this shape with `--shared-memory --import-memory`: data is initialized by a start function that only the first instance runs.

Each guest thread is its own instance of the module on the shared memory, in its own wasmtime store, on its own host thread. The instance's thread-local block holds the thread's guest registers, its exclusive monitor and its dispatch cache, so translated code runs without any per-thread indirection other than `__tls_base`. At `clone(CLONE_THREAD | CLONE_VM | CLONE_SIGHAND, ...)` the support code writes a *clone frame*: the parent's registers, with x0 = 0, SP = the new stack, TPIDR_EL0 = the `tls` argument (with `CLONE_SETTLS`) and the address after the `svc` to resume at. It passes the frame's address to the runtime's `clone` as a sixth argument. The runtime allocates a tid, honours `CLONE_PARENT_SETTID`, `CLONE_CHILD_SETTID` and `CLONE_CHILD_CLEARTID`, instantiates the module again, gives the instance a thread-local block and a 64 KB C stack from the mmap region, and calls `__wasm_env_thread_start(frame)`. The new thread copies its registers and makes the wasm-env call `thread_started`; only then does `clone` return the tid to the parent, which may reuse the frame.

Atomics: load-acquire, store-release and the LSE atomics become WebAssembly atomics (sequentially consistent, so at least as strong as arm64 asks); `dmb`/`dsb` become `atomic.fence`. A load-exclusive records the value it read, and a store-exclusive is a compare-and-swap against that value, as QEMU does it (a store of the same value in between goes unnoticed). Ordinary loads and stores are ordinary WebAssembly accesses.

What threads share: memory, the descriptor table, the working directory, signal dispositions, the address space (`mmap`, `brk`), futexes and pending signals (a signal to the process is taken by a thread that doesn't block it, at its next syscall). Each thread has its own signal mask and alternate signal stack; the umask is per thread too, copied from the creating thread. `getpid` returns the process id; `gettid` the thread's (the first thread's is the pid). `sched_getaffinity`, `/proc/cpuinfo` and `/sys/devices/system/cpu/{online,possible,present}` report the host's CPUs.

`futex` supports `FUTEX_WAIT`, `FUTEX_WAKE`, `FUTEX_WAIT_BITSET`, `FUTEX_WAKE_BITSET`, `FUTEX_REQUEUE` and `FUTEX_CMP_REQUEUE` (private or not: processes don't share memory); a wait returns `-EINTR` when a signal arrives or the process is ending. `exit` ends the calling thread (clearing its tid word and waking a waiter on it, for `pthread_join`); when the last thread exits, the process exits with its status. `exit_group`, a fatal signal, a trap in any thread or `execve` end the whole process: the other threads stop at their next syscall, and blocking calls return early. (A thread that never makes another syscall keeps running until the environment exits.)

## Libraries

A translated program's shared libraries are **modules of their own**, translated once per library and shared by every program that uses it, as on Linux. (`WASM_ENV_MONOLITHIC=1` translates a program together with its libraries in one module instead, the earlier way.)

- The **main module** is the executable alone, with the translator's support code. It exports, besides the items under "Threads": `__indirect_function_table`, the guest-state global `__wasm_env_state`, the support code's entry points (`dispatch`, `__wasm_env_svc`, `__wasm_env_svc_at`, `__wasm_env_fp_flags`, `__wasm_env_fatal`, `__wasm_env_bad_target`, `__wasm_env_crc32`, `__wasm_env_counter`) and the module directory (`__wasm_env_modules`, `__wasm_env_module_count`, `__wasm_env_modules_added`, and the code epoch `__wasm_env_code_epoch`, see "Code generated at run time"), and its region table (`dispatch_addrs`, `dispatch_fns`, `dispatch_count`, see "Blocks a translation missed").
- A **library module** is a position-independent side module in wasm-ld's dynamic-linking convention (`dylink.0`; `--experimental-pic -shared`). It imports the shared memory, the function table, `__stack_pointer`, `__memory_base`, `__table_base`, `__wasm_env_state` and the support code's entry points from `env`, and a `GOT.func` global per function whose address its data holds (its region functions). It exports its region table (`dispatch_addrs`, `dispatch_fns`, `dispatch_count`, relative to `__memory_base`), and a custom section `wasm-env.library` records the guest range it was translated for (two u64s, start and end).

**Guest state** is a block per thread (`struct guest_state` in the support code: registers, flags, the exclusive monitor and the dispatch cache), at the address in the WebAssembly global `__wasm_env_state`, which every module of a thread shares. Each region function reads it once.

**Addresses.** Each library is translated for a fixed guest range, which the runtime keeps from one translation to the next in its cache (`library-bases`), from 64 GB upward: every library has its own range, so any set of them fits in one process. The executable keeps its own addresses.

**Loading.** At start, the runtime loads the program's dynamic linker (PT_INTERP) as the kernel would: it maps its segments at its range and loads its module, and adds `AT_BASE` and a wasm-env entry `0x7700` (where to start: the dynamic linker's entry) to the auxiliary vector. When the dynamic linker then maps a library, the support code asks the runtime with the wasm-env call `load_library(fd, range)`: if the file is an arm64 shared object, the runtime translates it if it hasn't yet, loads its module and writes its guest range to `range` (else `-ENOEXEC`, and the file is mapped as data). The support code places the dynamic linker's mappings of the library in that range, as with the monolithic translation's prelinked bases.

Loading a module into a process: the runtime allocates its data (`dylink.0`'s size and alignment) in the mmap region and its function-table slots after the main module's, instantiates it against the main instance's exports (filling each `GOT.func` global with its slot, and the slot with the exported function), runs `__wasm_apply_data_relocs` once, and adds an entry `{start, end, region addresses, region functions, count, generation}` to the module directory, in a free slot (one a retired module left, else the next; `__wasm_env_module_count` is the number of slots used), writing the range last, and increments `__wasm_env_modules_added`. `dispatch` searches the directory for addresses outside the main module. Each thread has instances of the modules in its own store, with the same placements; a thread that finds a module when more have been added than it has caught up with asks the runtime to catch up (the wasm-env call `sync_modules`), and a new thread instantiates all loaded modules before it starts. Libraries stay loaded until the process exits (`dlclose` unmaps nothing).

## Code generated at run time

A JIT (oneDNN's Xbyak, LuaJIT) writes machine code into memory and jumps to it. The runtime records the memory a program makes executable: an anonymous `mmap` with `PROT_EXEC`, or `mprotect` adding it. When control reaches an address no module covers, `dispatch` asks the runtime (the wasm-env call `translate_code(address)`) to translate the executable mapping holding it, as it is then, from that address: the translator's `--raw` input, code bytes at a base with the addresses it was entered at as block roots. Such code, and a page translated again (see "Blocks a translation missed"), is compiled straight from the lifter's IR to WebAssembly by the translator's own compiler (translator/src/wasmgen.rs), in the runtime's process (about a millisecond, where clang and wasm-ld take a tenth of a second or more), which makes the same side module; what it doesn't handle yet (vector arithmetic, some intrinsics; lane access, bitcasts between vectors and registers, and structure loads and stores, `ld3`'s `<24 x i8>` kept in two v128s and split by `i8x16.shuffle`, it does) goes to clang, at `-O1` (the program waits). The result is a module like a library's, added to the module directory the same way, covering only the whole pages its blocks occupy, and cached by bytes, base and roots (a JIT that chooses its addresses generates the same code run after run).

**Chaining.** A translation covers only the code reached from the address it was made for, following control flow within that address's page; it doesn't translate the page's other code again. Several modules may therefore cover a page, and `dispatch` takes the newest whose region table has the region. An address a module has no block for goes on to the older code: its region function's missed case tail-calls, through the table slot its import `prev.R_<region>` holds, the function of the same region in the next older live module, and the chain ends at a slot holding the support code's `__wasm_env_bad_target`, which translates that entry. A new module sharing a page with older code bumps the code epoch, so dispatch caches drop the older function. Modules sharing a page all keep that page's bytes, so a change to it retires them together. (What the direct compiler doesn't handle is compiled by clang, whose module doesn't chain: it takes the entries of the modules on its pages and replaces them.)


**Retiring.** A module keeps the bytes it was translated from. When the program makes memory executable again (a JIT that changes or adds code makes it writable first), modules there whose bytes changed are retired; unmapping or remapping memory retires its modules outright. Retiring empties the module's directory entry (the slot is reused) and increments `__wasm_env_code_epoch`; each thread clears its dispatch cache, which may name the old code, when it sees a new epoch (after a system call, and in `dispatch`). Code changed while it stays writable and executable, without an `mprotect`, is noticed the way the processor would: code written as data runs only after `ic ivau` over its cache lines and an `isb`, which a JIT does itself or through `__builtin___clear_cache`. The lifted `ic ivau` notes its line (in the support code), and the next `isb` has the runtime compare the translations of those lines with the bytes now (the wasm-env call `code_changed`), retiring those that differ; the `isb` also checks the code epoch, so it sees other threads' changes too.

## Blocks a translation missed

Discovery can't find every block: an interpreter may reach its handlers through a table it builds at run time, as LuaJIT's does. When control reaches an address its translation (the program's, or a library module's) has no block for, the support code asks the runtime with `translate_code(address, object, base)` (the object and base from the program's object list, or zero for a library module, which the runtime knows). The runtime records the address in the module's `.roots` file, so the next translation includes it as a block root, and translates that one page again right away: the translator's `--page` option, the object translated as before (same mode and base) with every root recorded so far, emitting only that page's region, whose jumps to other regions go through `dispatch`. The new module takes over the page's entry in the owner's region table (the main module exports its table: `dispatch_addrs`, `dispatch_fns`, `dispatch_count`), and is also added to the module directory, where `dispatch` finds it if the translation had no region on that page (`dispatch` searches past a module whose table lacks the region). An earlier patch of the page is retired, and the code epoch bumped either way. So the run goes on: a program whose translation misses blocks no longer stops at each one. Around a recorded address, discovery also takes the instruction after each unconditional branch or return as a block start, so one missed handler reveals its neighbours. (Not for a program translated together with its dynamic linker, whose image isn't one object's: that stops, and the next run translates the address.)

## `setjmp` and `longjmp`

The compiler rewrites `setjmp`/`longjmp` using WebAssembly exception handling (`-mllvm -wasm-enable-sjlj`): `longjmp` throws the C tag `__c_longjmp`, and functions that call `setjmp` catch it and check whether it's theirs. libc supplies the three helpers the compiler calls (`__wasm_setjmp`, `__wasm_setjmp_test`, `__wasm_longjmp`). Everything linked into a program must be compiled with the same flags.

## Processes

Processes are created with **`vfork` + `execve`**, which covers `posix_spawn`, `system`, `popen` and shells; translated programs can also `fork` (see "fork (translated programs)" below).

**vfork.** `clone(CLONE_VM | CLONE_VFORK | SIGCHLD, 0)` (no new stack) enters a *vfork child*:

1. The runtime allocates the child's pid and saves the parent's process state (descriptor table, working directory, umask, signal dispositions). The child continues **in the parent's memory, on the parent's call stack**, with its own copy of that state, exactly as a real vfork child shares its parent's memory.
2. When the child calls `execve` and it succeeds, the runtime starts the new program as the child's pid, restores the parent's state, and **returns the child's pid** (instead of not returning).
3. When the child calls `exit_group`, the runtime records the child's exit status, restores the parent's state, and **returns the child's pid**.
4. In both cases libc then `longjmp`s back to the `vfork` call site, where `vfork()` returns the pid to the parent.

`vfork` returns twice, so libc defines it as a macro around `setjmp` (see `toolchain/musl-overlay/src/process/wasm64/vfork.c`); the child must not return from the function that called `vfork`, the usual vfork rule. `clone` with any other flags returns `-ENOSYS`.

Translated (arm64) programs use the same protocol through the translator's support code, so their own libc's `vfork`, `posix_spawn` and `system` work: at the guest's `clone(CLONE_VM | CLONE_VFORK, stack)` the support code saves the guest registers and the address after the `svc`, and the child continues (on `stack` if one is given, as glibc's `posix_spawn` does). When the runtime returns the child's pid from its `execve` or `exit`, the support code restores the registers, puts the pid in x0 and resumes the parent after its `clone`.

**execve** accepts WebAssembly programs, `#!` scripts (up to four levels of interpreter) and arm64 ELF binaries, which the runtime translates on first use (with their dynamic linker and libraries from the environment) and caches. Other files fail with `-ENOEXEC`. Outside a vfork child, a successful `execve` replaces the running program; the pid, descriptors (minus close-on-exec ones), working directory, umask and ignored signals carry over.

**Process model.** Each process runs on its own host thread, all inside the environment's one host process. `wait4` supports the usual targets (-1, a pid, 0, -pgid) and `WNOHANG`, and reports standard Linux wait statuses. Pipes are in-runtime buffers of 64 KB; writing to a pipe without readers raises `SIGPIPE`.

**Signals.** `kill`, `tkill` and `tgkill` queue a signal for the process (a set of pending signals; threads share them); it's acted on at a syscall of one of its threads that doesn't block it (`rt_sigprocmask`, per thread, kept across exec), and blocking calls (`read` on a pipe, `wait4`, `nanosleep`, `futex`, `poll`) return `-EINTR` when one arrives that would do something. Default actions apply (terminate, or ignore for SIGCHLD, SIGCONT, SIGURG and SIGWINCH), `SIG_IGN` is honored, and a child's exit sends its parent SIGCHLD. **Handlers run in translated programs**: when a syscall returns with a signal to deliver, the runtime writes the handler's details into the thread's guest-state block (`struct signal_delivery`), and the support code does what the arm64 kernel does: it pushes a `struct rt_sigframe` (siginfo, and a ucontext with the registers, NZCV, the FP/SIMD record and the old mask) on the stack, or the `sigaltstack` for `SA_ONSTACK`, and enters the handler with x0 = the signal, x1 = &siginfo, x2 = &ucontext and x30 = the `sa_restorer` (`SA_RESTORER`), else an address `dispatch` takes as `rt_sigreturn` (for glibc, which relies on the kernel's vDSO trampoline). `rt_sigreturn` restores the state from the frame. `SA_NODEFER`, `SA_RESETHAND`, `sa_mask`, `rt_sigsuspend`, `rt_sigpending` and `setitimer`/`getitimer` (`ITIMER_REAL`, SIGALRM) work. Not yet: `SA_RESTART` (interrupted calls return `-EINTR`, which Python and most libraries retry), signals interrupting code between syscalls, and handlers in programs built with the toolchain (their handled signals are ignored).

**Devices.** The runtime provides `/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr` and `/dev/fd/N`, whatever the root directory contains.
It also generates `/proc/cpuinfo`, `/sys/devices/system/cpu/{online,possible,present}` (the host's CPUs), `/proc/mounts`, `/proc/self/mounts`, `/etc/mtab` and `/proc/self/mountinfo` (the root, and `/proc`).

**Networking.** Sockets are the host's: a program reaches the network, and binds ports, as the host would. Supported: `AF_INET` and `AF_INET6` sockets, stream and datagram (`SOCK_NONBLOCK` and `SOCK_CLOEXEC` too); `connect` (a non-blocking one reports `-EINPROGRESS`, then its result through `SO_ERROR`), `bind`, `listen`, `accept`/`accept4`, `getsockname`, `getpeername`, `sendto`, `recvfrom`, `sendmsg` and `recvmsg` (no control messages), `sendmmsg`, `shutdown`, `read`/`write` on sockets, and `setsockopt`/`getsockopt` for the common options (others are accepted and ignored). Blocking follows the descriptor's `O_NONBLOCK` (`fcntl` or `ioctl(FIONBIO)`). `ppoll` and `pselect6` work across sockets, pipes and files (files are always ready). Other address families (`AF_UNIX`, `AF_NETLINK`) return `-EAFNOSUPPORT`; glibc copes (no nscd; `getaddrinfo` assumes both IP families). Names resolve as the guest's libc does, from the root's `/etc/hosts` and `/etc/resolv.conf`; if the root's `/etc/resolv.conf` is missing or empty, as in a fresh image, the runtime provides the host's (as Docker does for containers). Writing to a broken connection returns `-EPIPE` without raising `SIGPIPE`.

**fork (translated programs).** A translated program's state at a syscall is all in its memory: the guest registers are in the thread's guest-state block, and nothing on the host stack matters (the lifted code reaches the syscall through tail calls). So `clone` without `CLONE_VM` or `CLONE_THREAD` makes the child a copy: a new process whose memory holds the parent's pages (those in use: the image, break and mmap region, and libraries; untouched pages stay unallocated), with copies of the descriptor table, working directory, credentials and signal dispositions, running new instances of the program and its libraries with the same placements. The support code passes a clone frame (see "Threads"); the runtime points the child's instance at the forking thread's thread-local block and C stack (`__tls_base`, `__stack_pointer`, both exported) and calls `__wasm_env_fork_start(frame)`, which resumes after the `clone` with x0 = 0. `CLONE_PARENT_SETTID`, `CLONE_CHILD_SETTID` and `CLONE_CHILD_CLEARTID` apply. Only the forking thread exists in the child, as on Linux. A fork takes a few milliseconds.

**Credentials.** A process has user and group ids (real, effective, saved) and supplementary groups, starting as root; the `set*id`, `get*id`, `setgroups` and `getgroups` calls change and report them, and they pass through fork, vfork and exec. They are only numbers: files belong to the host user, and permissions aren't checked.

**Terminals.** When the runtime's stdin, stdout or stderr is a terminal, `TCGETS`, `TCSETS*`, `TIOCGWINSZ`, `TCXONC` and `TCFLSH` on them go to the host terminal (on Linux hosts: the structures have the same layout), and `TIOCGPGRP`/`TIOCSPGRP` read and set the environment's foreground process group. Ctrl-C and Ctrl-\ at the host terminal send SIGINT and SIGQUIT to that group; the terminal's settings are restored when wasm-env exits.

**Locks.** `fcntl`'s record locks (`F_GETLK`, `F_SETLK`, `F_SETLKW` and the `F_OFD_` forms) and `flock` are the host's open-file-description locks: the environment's processes share one host process, whose per-process POSIX locks would never conflict between them.

**fork (programs built with the toolchain)**, which keep C state on the host stack, remains open (they have vfork). The options:

| Option | How it works | Trade-off |
|---|---|---|
| Asyncify-style unwinding | The compiler instruments code so the call stack can be saved to memory and rebuilt in a copy of the process | Works on any runtime; code grows and slows (roughly 10–50%) |
| Stack switching | Use the WebAssembly stack-switching proposal to capture the running stack | Cheap, but the proposal isn't finished and wasmtime support is experimental |
| Runtime-level snapshot | wasmtime copies the instance, including its native stack | No compiler changes; needs deep wasmtime changes |
**Environments.** `wasm-env convert IMAGE DIR` turns an arm64 Linux image (an OCI reference pulled from its registry, an OCI layout, or a root filesystem tarball or directory) into an environment: its root filesystem, with every arm64 program and library translated into `/var/cache/wasm-env` inside it (`--lazy`: on first use instead). Translations are keyed by content and by the translator's version, and libraries in the image are registered by their path in it, so an environment can move. `wasm-env shell DIR` runs a login shell in it; `wasm-env export DIR OUT[.tar]` writes it as an OCI image layout for platform wasi/wasm (one layer, translations included). Translating needs a toolchain (clang, wasm-ld, compiler-rt builtins for wasm64): an installed wasm-env has one in `lib/wasm-env/toolchain` next to its binaries (`$WASM_ENV_TOOLCHAIN` overrides), and the dev shell provides it through `$WASM_CC`.


## Toolchain

- Target: `clang --target=wasm64-unknown-unknown`, wrapped by `wasm64-linux-musl-cc`, which sets the sysroot and flags and defines `__linux__` and `__unix__` (but not `__gnu_linux__`, since this is musl). clang has no `wasm64-*-linux` target, so the wrapper stands in for one.
- `long double` is 128-bit IEEE quad, as on arm64 Linux, emulated in software (compiler-rt builtins).
- Linking is static: `crt1.o`, the program, `libc.a` and compiler-rt builtins, linked by `wasm-ld`. Dynamic linking is out of scope.

## Planned versions

- **Version 1: dynamic linking** (PLAN.md, M3). Several modules in one process, sharing one memory and one function table, loaded and linked by the runtime at run time, including `dlopen`. Needed so translated programs can load shared libraries such as Python extension modules. Translated modules use glibc's symbol versions. (Translated programs already have them; see "Libraries".)
- **Version 2: threads** (PLAN.md, M6). Shared memory, atomics and `clone(CLONE_THREAD)`, with real thread-local storage, for programs built with the toolchain. (Translated programs already have them; see "Threads".)

## wasm-env's own calls

Syscall numbers from 2^32 up are wasm-env's, not Linux's; unknown ones return `-ENOSYS` like any other.

| Number | Call | Arguments |
|---|---|---|
| 2^32 | `missed_target` | (address, object name, offset in the object). Made by a translated program's support code when it jumps to code the translation missed and `translate_code` can't translate it then (see "Blocks a translation missed"). The runtime records the address, and the next translation of that program includes it. |
| 2^32 + 1 | `thread_started` | (). Made by a new thread of a translated program once it has taken its registers from its clone frame (see "Threads"). |
| 2^32 + 2 | `load_library` | (fd, range). Made by a translated program's support code when the dynamic linker maps a file: loads it as a library module if it is one, writing its guest start and end at `range` (see "Libraries"); else `-ENOEXEC`. |
| 2^32 + 3 | `sync_modules` | (). Made when a thread reaches a library module its instance hasn't loaded yet (see "Libraries"). |
| 2^32 + 4 | `translate_code` | (address, object name, object base). Made when control reaches an address no translated block covers: for code the program generated, translates the executable memory holding it (see "Code generated at run time"); for a block the program's or a library's translation missed, translates its page again (see "Blocks a translation missed"; the object is the program's, or zero for a library module). 0 if it translated something, 1 if the address was translated already (the thread's dispatch cache named older code), `-ENOEXEC` if it can't. |
| 2^32 + 5 | `code_changed` | (start, length). Made at an `isb` after `ic ivau` (instruction cache maintenance, after writing code): translations of code in that range whose bytes changed are retired (see "Code generated at run time"). |

## Versioning

The module declares the ABI version it was built for in a custom section named `wasm-env.abi`, containing the version number as a LEB128 integer. Version 0 is this document. The runtime refuses versions it doesn't know.
