//! Linux constants for the wasm-env ABI: the generic (arm64) syscall table,
//! errno values and flags. See docs/abi.md.

/// Syscall numbers (include/uapi/asm-generic/unistd.h).
pub mod nr {
    pub const GETCWD: i64 = 17;
    pub const DUP: i64 = 23;
    pub const DUP3: i64 = 24;
    pub const FCNTL: i64 = 25;
    pub const IOCTL: i64 = 29;
    pub const MKDIRAT: i64 = 34;
    pub const UNLINKAT: i64 = 35;
    pub const SYMLINKAT: i64 = 36;
    pub const LINKAT: i64 = 37;
    pub const RENAMEAT: i64 = 38;
    pub const FTRUNCATE: i64 = 46;
    pub const FALLOCATE: i64 = 47;
    pub const FCHMOD: i64 = 52;
    pub const FCHMODAT: i64 = 53;
    pub const FCHOWNAT: i64 = 54;
    pub const FCHOWN: i64 = 55;
    pub const FACCESSAT: i64 = 48;
    pub const FACCESSAT2: i64 = 439;
    pub const CHDIR: i64 = 49;
    pub const FCHDIR: i64 = 50;
    pub const OPENAT: i64 = 56;
    pub const CLOSE: i64 = 57;
    pub const PIPE2: i64 = 59;
    pub const GETDENTS64: i64 = 61;
    pub const LSEEK: i64 = 62;
    pub const READ: i64 = 63;
    pub const WRITE: i64 = 64;
    pub const READV: i64 = 65;
    pub const WRITEV: i64 = 66;
    pub const PREAD64: i64 = 67;
    pub const PWRITE64: i64 = 68;
    pub const READLINKAT: i64 = 78;
    pub const NEWFSTATAT: i64 = 79;
    pub const FSTAT: i64 = 80;
    pub const FSYNC: i64 = 82;
    pub const FDATASYNC: i64 = 83;
    pub const UTIMENSAT: i64 = 88;
    pub const EXIT: i64 = 93;
    pub const EXIT_GROUP: i64 = 94;
    /// wasm-env's own calls start at 2^32, far from Linux's. This one is made
    /// by translated programs' support code when they jump to an address that
    /// wasn't translated: (pc, object name, offset in the object).
    pub const WASM_ENV_MISSED_TARGET: i64 = 1 << 32;
    /// Made by a new thread of a translated program once it has taken its
    /// registers from the clone frame (see thread::spawn).
    pub const WASM_ENV_THREAD_STARTED: i64 = (1 << 32) + 1;
    /// Made by translated programs' support code when the dynamic linker maps
    /// a library, and when a thread reaches one its instance hasn't loaded
    /// yet (see modules.rs).
    pub const WASM_ENV_LOAD_LIBRARY: i64 = (1 << 32) + 2;
    pub const WASM_ENV_SYNC_MODULES: i64 = (1 << 32) + 3;
    /// Translate code the program generated at run time (see modules::translate_code).
    pub const WASM_ENV_TRANSLATE_CODE: i64 = (1 << 32) + 4;
    /// Code in [start, start + len) may have changed (see modules::check_code).
    pub const WASM_ENV_CODE_CHANGED: i64 = (1 << 32) + 5;
    /// Translate the exact-FP version of a page (see modules::exact_fp).
    pub const WASM_ENV_EXACT_FP: i64 = (1 << 32) + 6;
    pub const SET_TID_ADDRESS: i64 = 96;
    pub const SET_ROBUST_LIST: i64 = 99;
    pub const FUTEX: i64 = 98;
    pub const NANOSLEEP: i64 = 101;
    pub const CLOCK_NANOSLEEP: i64 = 115;
    pub const CLOCK_GETTIME: i64 = 113;
    pub const SCHED_GETAFFINITY: i64 = 123;
    pub const KILL: i64 = 129;
    pub const TKILL: i64 = 130;
    pub const TGKILL: i64 = 131;
    pub const SIGALTSTACK: i64 = 132;
    pub const RT_SIGACTION: i64 = 134;
    pub const RT_SIGPROCMASK: i64 = 135;
    pub const RT_SIGPENDING: i64 = 136;
    pub const RT_SIGSUSPEND: i64 = 133;
    pub const GETITIMER: i64 = 102;
    pub const SETITIMER: i64 = 103;
    pub const SETPGID: i64 = 154;
    pub const GETPGID: i64 = 155;
    pub const GETSID: i64 = 156;
    pub const SETSID: i64 = 157;
    pub const UNAME: i64 = 160;
    pub const UMASK: i64 = 166;
    pub const GETTIMEOFDAY: i64 = 169;
    pub const GETPID: i64 = 172;
    pub const GETPPID: i64 = 173;
    pub const GETUID: i64 = 174;
    pub const GETEUID: i64 = 175;
    pub const GETGID: i64 = 176;
    pub const GETEGID: i64 = 177;
    pub const GETTID: i64 = 178;
    pub const BRK: i64 = 214;
    pub const MUNMAP: i64 = 215;
    pub const MREMAP: i64 = 216;
    pub const CLONE: i64 = 220;
    pub const EXECVE: i64 = 221;
    pub const MMAP: i64 = 222;
    pub const MPROTECT: i64 = 226;
    pub const MSYNC: i64 = 227;
    pub const MADVISE: i64 = 233;
    pub const WAIT4: i64 = 260;
    pub const TIMES: i64 = 153;
    pub const GETRUSAGE: i64 = 165;
    pub const PRLIMIT64: i64 = 261;
    pub const RENAMEAT2: i64 = 276;
    pub const MEMFD_CREATE: i64 = 279;
    pub const GETRANDOM: i64 = 278;
    pub const PSELECT6: i64 = 72;
    pub const PPOLL: i64 = 73;
    pub const EVENTFD2: i64 = 19;
    pub const INOTIFY_INIT1: i64 = 26;
    pub const INOTIFY_ADD_WATCH: i64 = 27;
    pub const INOTIFY_RM_WATCH: i64 = 28;
    pub const PRCTL: i64 = 167;
    pub const FADVISE64: i64 = 223;
    pub const EPOLL_CREATE1: i64 = 20;
    pub const EPOLL_CTL: i64 = 21;
    pub const EPOLL_PWAIT: i64 = 22;
    pub const EPOLL_PWAIT2: i64 = 441;
    pub const CLOCK_GETRES: i64 = 114;
    pub const SOCKET: i64 = 198;
    pub const SOCKETPAIR: i64 = 199;
    pub const BIND: i64 = 200;
    pub const LISTEN: i64 = 201;
    pub const ACCEPT: i64 = 202;
    pub const CONNECT: i64 = 203;
    pub const GETSOCKNAME: i64 = 204;
    pub const GETPEERNAME: i64 = 205;
    pub const SENDTO: i64 = 206;
    pub const RECVFROM: i64 = 207;
    pub const SETSOCKOPT: i64 = 208;
    pub const GETSOCKOPT: i64 = 209;
    pub const SHUTDOWN: i64 = 210;
    pub const SENDMSG: i64 = 211;
    pub const RECVMSG: i64 = 212;
    pub const ACCEPT4: i64 = 242;
    pub const SENDMMSG: i64 = 269;
    pub const SETXATTR: i64 = 5;
    pub const LSETXATTR: i64 = 6;
    pub const FSETXATTR: i64 = 7;
    pub const GETXATTR: i64 = 8;
    pub const LGETXATTR: i64 = 9;
    pub const FGETXATTR: i64 = 10;
    pub const LISTXATTR: i64 = 11;
    pub const LLISTXATTR: i64 = 12;
    pub const FLISTXATTR: i64 = 13;
    pub const REMOVEXATTR: i64 = 14;
    pub const LREMOVEXATTR: i64 = 15;
    pub const FREMOVEXATTR: i64 = 16;
    pub const STATFS: i64 = 43;
    pub const FSTATFS: i64 = 44;
    pub const MINCORE: i64 = 232;
    pub const STATX: i64 = 291;
    pub const FLOCK: i64 = 32;
    pub const SETREGID: i64 = 143;
    pub const SETGID: i64 = 144;
    pub const SETREUID: i64 = 145;
    pub const SETUID: i64 = 146;
    pub const SETRESUID: i64 = 147;
    pub const GETRESUID: i64 = 148;
    pub const SETRESGID: i64 = 149;
    pub const GETRESGID: i64 = 150;
    pub const SETFSUID: i64 = 151;
    pub const SETFSGID: i64 = 152;
    pub const GETGROUPS: i64 = 158;
    pub const SETGROUPS: i64 = 159;
}

/// Syscall name for tracing.
pub fn name(n: i64) -> &'static str {
    use nr::*;
    match n {
        GETCWD => "getcwd", DUP => "dup", DUP3 => "dup3", FCNTL => "fcntl", IOCTL => "ioctl",
        MKDIRAT => "mkdirat", UNLINKAT => "unlinkat", SYMLINKAT => "symlinkat", LINKAT => "linkat",
        RENAMEAT => "renameat", RENAMEAT2 => "renameat2", FTRUNCATE => "ftruncate", FCHMOD => "fchmod",
        FCHMODAT => "fchmodat", FCHOWNAT => "fchownat", FCHOWN => "fchown", FSYNC => "fsync",
        FDATASYNC => "fdatasync", FACCESSAT => "faccessat", FACCESSAT2 => "faccessat2",
        CHDIR => "chdir", FCHDIR => "fchdir", OPENAT => "openat", CLOSE => "close",
        GETDENTS64 => "getdents64", LSEEK => "lseek", READ => "read", WRITE => "write",
        READV => "readv", WRITEV => "writev", PREAD64 => "pread64", PWRITE64 => "pwrite64",
        READLINKAT => "readlinkat", NEWFSTATAT => "newfstatat", FSTAT => "fstat", UTIMENSAT => "utimensat",
        EXIT => "exit", EXIT_GROUP => "exit_group", SET_TID_ADDRESS => "set_tid_address",
        SET_ROBUST_LIST => "set_robust_list", NANOSLEEP => "nanosleep", CLOCK_NANOSLEEP => "clock_nanosleep",
        CLOCK_GETTIME => "clock_gettime", SCHED_GETAFFINITY => "sched_getaffinity",
        KILL => "kill", TGKILL => "tgkill", SIGALTSTACK => "sigaltstack",
        RT_SIGACTION => "rt_sigaction", RT_SIGPROCMASK => "rt_sigprocmask", UNAME => "uname",
        UMASK => "umask", GETTIMEOFDAY => "gettimeofday", GETPID => "getpid",
        GETPPID => "getppid", GETUID => "getuid", GETEUID => "geteuid", GETGID => "getgid",
        GETEGID => "getegid", GETTID => "gettid", BRK => "brk", MUNMAP => "munmap",
        MREMAP => "mremap", MMAP => "mmap", MPROTECT => "mprotect", FALLOCATE => "fallocate", MSYNC => "msync", INOTIFY_INIT1 => "inotify_init1", INOTIFY_ADD_WATCH => "inotify_add_watch", INOTIFY_RM_WATCH => "inotify_rm_watch", PRCTL => "prctl", FADVISE64 => "fadvise64", MEMFD_CREATE => "memfd_create", MADVISE => "madvise",
        PRLIMIT64 => "prlimit64", GETRANDOM => "getrandom", PIPE2 => "pipe2", CLONE => "clone",
        EXECVE => "execve", WAIT4 => "wait4", SETPGID => "setpgid", GETPGID => "getpgid",
        GETSID => "getsid", SETSID => "setsid", PSELECT6 => "pselect6", PPOLL => "ppoll",
        EVENTFD2 => "eventfd2", EPOLL_CREATE1 => "epoll_create1", EPOLL_CTL => "epoll_ctl", EPOLL_PWAIT => "epoll_pwait",
        EPOLL_PWAIT2 => "epoll_pwait2", CLOCK_GETRES => "clock_getres",
        SOCKET => "socket", SOCKETPAIR => "socketpair", BIND => "bind", LISTEN => "listen", ACCEPT => "accept",
        CONNECT => "connect", GETSOCKNAME => "getsockname", GETPEERNAME => "getpeername",
        SENDTO => "sendto", RECVFROM => "recvfrom", SETSOCKOPT => "setsockopt",
        GETSOCKOPT => "getsockopt", SHUTDOWN => "shutdown", SENDMSG => "sendmsg",
        RECVMSG => "recvmsg", ACCEPT4 => "accept4", SENDMMSG => "sendmmsg", STATX => "statx",
        STATFS => "statfs", FSTATFS => "fstatfs", MINCORE => "mincore", GETXATTR => "getxattr", FLOCK => "flock",
        LGETXATTR => "lgetxattr", FGETXATTR => "fgetxattr", LISTXATTR => "listxattr", LLISTXATTR => "llistxattr",
        _ => "?",
    }
}

/// errno values (include/uapi/asm-generic/errno-base.h and errno.h).
pub mod errno {
    pub const ENOENT: i64 = 2;
    pub const ESRCH: i64 = 3;
    pub const EINTR: i64 = 4;
    pub const EIO: i64 = 5;
    pub const E2BIG: i64 = 7;
    pub const ENOEXEC: i64 = 8;
    pub const EBADF: i64 = 9;
    pub const ECHILD: i64 = 10;
    pub const EAGAIN: i64 = 11;
    pub const ETIMEDOUT: i64 = 110;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EEXIST: i64 = 17;
    pub const EXDEV: i64 = 18;
    pub const ENODEV: i64 = 19;
    pub const ENOTDIR: i64 = 20;
    pub const EISDIR: i64 = 21;
    pub const EINVAL: i64 = 22;
    pub const EMFILE: i64 = 24;
    pub const ENOTTY: i64 = 25;
    pub const ESPIPE: i64 = 29;
    pub const EROFS: i64 = 30;
    pub const EPIPE: i64 = 32;
    pub const ERANGE: i64 = 34;
    pub const ENAMETOOLONG: i64 = 36;
    pub const ENOSYS: i64 = 38;
    pub const ENOTEMPTY: i64 = 39;
    pub const ELOOP: i64 = 40;
    pub const ENODATA: i64 = 61;
    pub const ENOTSUP: i64 = 95;
}

/// openat flags (arm64 values, arch/aarch64/bits/fcntl.h).
pub mod o {
    pub const ACCMODE: i64 = 0o3;
    pub const WRONLY: i64 = 0o1;
    pub const CREAT: i64 = 0o100;
    pub const EXCL: i64 = 0o200;
    pub const TRUNC: i64 = 0o1000;
    pub const APPEND: i64 = 0o2000;
    pub const NONBLOCK: i64 = 0o4000;
    pub const DIRECTORY: i64 = 0o40000;
    pub const NOFOLLOW: i64 = 0o100000;
    pub const CLOEXEC: i64 = 0o2000000;
}

pub const AT_FDCWD: i64 = -100;
pub const AT_SYMLINK_NOFOLLOW: i64 = 0x100;
pub const AT_REMOVEDIR: i64 = 0x200;
pub const AT_EMPTY_PATH: i64 = 0x1000;

/// Auxiliary vector types.
pub mod at {
    pub const NULL: u64 = 0;
    pub const PAGESZ: u64 = 6;
    pub const BASE: u64 = 7;
    pub const UID: u64 = 11;
    pub const EUID: u64 = 12;
    pub const GID: u64 = 13;
    pub const EGID: u64 = 14;
    pub const HWCAP: u64 = 16;
    pub const SECURE: u64 = 23;
    pub const RANDOM: u64 = 25;
    pub const EXECFN: u64 = 31;
}

pub const PAGE_SIZE: u64 = 4096;
