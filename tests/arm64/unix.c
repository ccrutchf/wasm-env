// Unix sockets with paths, descriptor passing (SCM_RIGHTS), memfd and
// shared mappings (MAP_SHARED): what Wayland clients and GPU drivers need
// (PLAN.md, M8), and file locks between processes. Differential against
// qemu, so it prints no pids, addresses or host paths.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/inotify.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static void check(int ok, const char *what) {
    if (!ok) {
        printf("FAILED: %s (errno %d)\n", what, errno);
        exit(1);
    }
}

// Sends `fd` (and one byte) over the socket `s`.
static void send_fd(int s, int fd) {
    char byte = 'x';
    struct iovec iov = {&byte, 1};
    union { struct cmsghdr h; char buf[CMSG_SPACE(sizeof(int))]; } u;
    memset(&u, 0, sizeof u);
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = u.buf, .msg_controllen = sizeof u.buf};
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof fd);
    check(sendmsg(s, &m, 0) == 1, "sendmsg with a descriptor");
}

// Receives a descriptor sent by send_fd.
static int recv_fd(int s) {
    char byte;
    struct iovec iov = {&byte, 1};
    union { struct cmsghdr h; char buf[CMSG_SPACE(sizeof(int))]; } u;
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = u.buf, .msg_controllen = sizeof u.buf};
    check(recvmsg(s, &m, MSG_CMSG_CLOEXEC) == 1, "recvmsg with a descriptor");
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    check(c && c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS, "an SCM_RIGHTS message");
    check(c->cmsg_len == CMSG_LEN(sizeof(int)) && !(m.msg_flags & MSG_CTRUNC), "one descriptor, not truncated");
    int fd;
    memcpy(&fd, CMSG_DATA(c), sizeof fd);
    check((fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0, "MSG_CMSG_CLOEXEC");
    return fd;
}

static void memfd_and_mapping(void) {
    int fd = memfd_create("wasm-env-test", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    check(fd >= 0, "memfd_create");
    check(ftruncate(fd, 8192) == 0, "ftruncate");
    check(pwrite(fd, "written", 7, 100) == 7, "pwrite");
    char *p = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    check(p != MAP_FAILED, "mmap MAP_SHARED");
    printf("mapping sees the file: %.7s\n", p + 100);
    memcpy(p + 4096, "through the mapping", 19);
    char buf[20] = {0};
    check(pread(fd, buf, 19, 4096) == 19, "pread");
    printf("file sees the mapping: %s\n", buf);
    struct stat st;
    check(fstat(fd, &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 8192, "fstat");
    check(fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK) == 0, "F_ADD_SEALS");
    printf("seals: %s\n", fcntl(fd, F_GET_SEALS) == F_SEAL_SHRINK ? "shrink" : "?");
    printf("sealed against shrinking: %s\n", ftruncate(fd, 4096) < 0 && errno == EPERM ? "EPERM" : "no");
    // Unmapped, then mapped anonymously in its place: zeroes, not the file.
    check(munmap(p, 8192) == 0, "munmap");
    char *q = mmap(p, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    check(q == p, "mmap MAP_FIXED over it");
    printf("anonymous in its place: %d %d\n", q[100], q[4096]);
    munmap(q, 8192);
    close(fd);
}

static void shared_across_fork(void) {
    // Anonymous shared memory, and a memfd mapped shared: a child's writes
    // are the parent's.
    int *anon = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    check(anon != MAP_FAILED, "mmap MAP_SHARED | MAP_ANONYMOUS");
    int fd = memfd_create("fork", 0);
    check(fd >= 0 && ftruncate(fd, 4096) == 0, "memfd");
    int *file = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int *private = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    *anon = 1, *file = 1, *private = 1;
    pid_t pid = fork();
    check(pid >= 0, "fork");
    if (pid == 0) {
        *anon = 42, *file = 43, *private = 44;
        _exit(0);
    }
    int status;
    waitpid(pid, &status, 0);
    printf("after the child: shared anonymous %d, shared file %d, private %d\n", *anon, *file, *private);
    // Moved and grown: still the same file.
    check(ftruncate(fd, 8192) == 0, "ftruncate");
    int *moved = mremap(file, 4096, 8192, MREMAP_MAYMOVE);
    check(moved != MAP_FAILED, "mremap");
    moved[1024] = 7;
    int back;
    check(pread(fd, &back, 4, 4096) == 4, "pread");
    printf("after mremap: %d, and the file has %d\n", moved[0], back);
    close(fd);
}

static void passing_descriptors(void) {
    int sv[2];
    check(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0, "socketpair");
    pid_t pid = fork();
    check(pid >= 0, "fork");
    if (pid == 0) {
        close(sv[0]);
        // A memfd the child made and wrote through a mapping, to the parent.
        int fd = memfd_create("from-child", 0);
        check(fd >= 0 && ftruncate(fd, 4096) == 0, "child memfd");
        char *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        strcpy(p, "hello from the child");
        send_fd(sv[1], fd);
        close(fd);
        // A pipe's read end from the parent: read what it wrote.
        int r = recv_fd(sv[1]);
        char buf[32] = {0};
        check(read(r, buf, sizeof buf - 1) > 0, "read the passed pipe");
        struct stat st;
        check(fstat(r, &st) == 0, "fstat the passed pipe");
        printf("child: the pipe (%s) says: %s\n", S_ISFIFO(st.st_mode) ? "a fifo" : "?", buf);
        // Wait for the parent to write into the shared page.
        char go;
        check(read(sv[1], &go, 1) == 1, "read");
        printf("child: its memfd now says: %s\n", p);
        _exit(0);
    }
    close(sv[1]);
    int fd = recv_fd(sv[0]);
    struct stat st;
    check(fstat(fd, &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 4096, "fstat the passed memfd");
    char *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    check(p != MAP_FAILED, "mmap the passed memfd");
    printf("parent: the child's memfd says: %s\n", p);
    int pipefd[2];
    check(pipe(pipefd) == 0, "pipe");
    send_fd(sv[0], pipefd[0]);
    close(pipefd[0]);
    check(write(pipefd[1], "through a passed pipe", 21) == 21, "write the pipe");
    close(pipefd[1]);
    strcpy(p, "hello from the parent");
    check(write(sv[0], "g", 1) == 1, "write");
    int status;
    waitpid(pid, &status, 0);
    printf("child exited %d\n", WEXITSTATUS(status));
    close(sv[0]);
}

// A server bound to `addr` accepts one connection from a forked child and
// reads what it sends.
static void serve(const struct sockaddr_un *addr, socklen_t len, const char *what) {
    int s = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    check(s >= 0, "socket(AF_UNIX)");
    check(bind(s, (const struct sockaddr *)addr, len) == 0, "bind");
    check(listen(s, 1) == 0, "listen");
    pid_t pid = fork();
    check(pid >= 0, "fork");
    if (pid == 0) {
        int c = socket(AF_UNIX, SOCK_STREAM, 0);
        check(connect(c, (const struct sockaddr *)addr, len) == 0, "connect");
        check(write(c, "ping", 4) == 4, "write");
        _exit(0);
    }
    struct sockaddr_un peer;
    socklen_t plen = sizeof peer;
    int a = accept(s, (struct sockaddr *)&peer, &plen);
    check(a >= 0, "accept");
    char buf[8] = {0};
    check(read(a, buf, sizeof buf - 1) == 4, "read");
    struct sockaddr_un name;
    socklen_t nlen = sizeof name;
    check(getsockname(s, (struct sockaddr *)&name, &nlen) == 0, "getsockname");
    printf("%s: got %s; peer unnamed: %s; ", what, buf, plen == sizeof(sa_family_t) ? "yes" : "no");
    if (name.sun_path[0])
        printf("bound as %s\n", name.sun_path);
    else
        printf("abstract, %d bytes of name\n", (int)(nlen - sizeof(sa_family_t) - 1));
    int status;
    waitpid(pid, &status, 0);
    close(a);
    close(s);
}

static void path_sockets(void) {
    struct sockaddr_un addr = {.sun_family = AF_UNIX};
    strcpy(addr.sun_path, "unix-test.sock");
    unlink(addr.sun_path);
    serve(&addr, sizeof addr, "a path");
    struct stat st;
    printf("the socket file is a socket: %s\n", stat(addr.sun_path, &st) == 0 && S_ISSOCK(st.st_mode) ? "yes" : "no");
    unlink(addr.sun_path);
    // A path whose host counterpart (under wasm-env's root) is longer than
    // sun_path allows.
    const char *dir = "unix-test-a-directory-with-a-rather-long-name-to-make-the-host-path-long";
    mkdir(dir, 0755);
    snprintf(addr.sun_path, sizeof addr.sun_path, "%s/s", dir);
    unlink(addr.sun_path);
    serve(&addr, sizeof addr, "a long path");
    unlink(addr.sun_path);
    rmdir(dir);
    // An abstract name.
    struct sockaddr_un abs = {.sun_family = AF_UNIX};
    const char name[] = "wasm-env-unix-test";
    memcpy(abs.sun_path + 1, name, sizeof name - 1);
    serve(&abs, sizeof(sa_family_t) + 1 + sizeof name - 1, "an abstract name");
    // Connecting to nothing.
    struct sockaddr_un none = {.sun_family = AF_UNIX};
    strcpy(none.sun_path, "no-such-socket");
    int c = socket(AF_UNIX, SOCK_STREAM, 0);
    printf("connect to nothing: %s\n", connect(c, (struct sockaddr *)&none, sizeof none) < 0 && errno == ENOENT ? "ENOENT" : "?");
    close(c);
}

// inotify (the host's, on the files under the root) and prctl's thread name.
static void watching(void) {
    const char *dir = "unix-test-watched";
    mkdir(dir, 0755);
    int in = inotify_init1(IN_CLOEXEC);
    check(in >= 0, "inotify_init1");
    int wd = inotify_add_watch(in, dir, IN_CREATE | IN_DELETE);
    check(wd >= 0, "inotify_add_watch");
    char path[64];
    snprintf(path, sizeof path, "%s/new-file", dir);
    close(open(path, O_CREAT | O_WRONLY, 0644));
    unlink(path);
    // (Events may come in more than one read: hosts report them as they
    // notice them.)
    char buf[4096] __attribute__((aligned(8)));
    for (int seen = 0; seen < 2;) {
        ssize_t n = read(in, buf, sizeof buf);
        check(n > 0, "read(inotify)");
        for (char *p = buf; p < buf + n; seen++) {
            struct inotify_event *e = (struct inotify_event *)p;
            printf("inotify: %s %s (same watch: %s)\n", e->mask & IN_CREATE ? "created" : e->mask & IN_DELETE ? "deleted" : "?", e->name, e->wd == wd ? "yes" : "no");
            p += sizeof *e + e->len;
        }
    }
    check(inotify_rm_watch(in, wd) == 0, "inotify_rm_watch");
    close(in);
    rmdir(dir);
    check(prctl(PR_SET_NAME, "worker") == 0, "prctl(PR_SET_NAME)");
    printf("dumpable: %d\n", prctl(PR_GET_DUMPABLE));
}

// Record locks (a process's and F_OFD_ ones) and flock, between processes.
static int setlk(int fd, int cmd, short type, off_t start, off_t len) {
    struct flock l = {.l_type = type, .l_whence = SEEK_SET, .l_start = start, .l_len = len};
    return fcntl(fd, cmd, &l);
}

static void locking(void) {
    const char *path = "unix-test-locks";
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    check(fd >= 0, "open");
    check(setlk(fd, F_SETLK, F_WRLCK, 0, 10) == 0, "F_SETLK");
    check(setlk(fd, F_OFD_SETLK, F_WRLCK, 100, 10) == 0, "F_OFD_SETLK");
    check(setlk(fd, F_SETLK, F_WRLCK, 200, 1) == 0, "F_SETLK 200");
    int whole = open(path, O_RDWR);
    check(flock(whole, LOCK_EX) == 0, "flock");
    pid_t parent = getpid();
    pid_t child = fork();
    if (child == 0) {
        int own = open(path, O_RDWR);
        errno = 0;
        printf("lock: child's write lock in the parent's range: %s\n", setlk(own, F_SETLK, F_WRLCK, 5, 1) == 0 ? "taken" : errno == EAGAIN || errno == EACCES ? "refused" : "?");
        struct flock l = {.l_type = F_RDLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 1};
        check(fcntl(own, F_GETLK, &l) == 0, "F_GETLK");
        printf("lock: F_GETLK: %s at %ld+%ld by the parent: %s\n", l.l_type == F_WRLCK ? "write" : "?", (long)l.l_start, (long)l.l_len, l.l_pid == parent ? "yes" : "no");
        printf("lock: beyond it: %s\n", setlk(own, F_SETLK, F_WRLCK, 20, 10) == 0 ? "taken" : "refused");
        printf("lock: in the description's range: %s\n", setlk(own, F_OFD_SETLK, F_RDLCK, 105, 1) == 0 ? "taken" : "refused");
        printf("flock: another description: %s\n", flock(own, LOCK_EX | LOCK_NB) == 0 ? "taken" : errno == EWOULDBLOCK ? "would block" : "?");
        printf("flock: the same description: %s\n", flock(whole, LOCK_EX | LOCK_NB) == 0 ? "taken" : "refused");
        check(setlk(own, F_SETLKW, F_WRLCK, 200, 1) == 0, "F_SETLKW");
        printf("lock: waited for the parent's\n");
        _exit(0);
    }
    usleep(200 * 1000);
    printf("lock: parent unlocks\n");
    check(setlk(fd, F_SETLK, F_UNLCK, 200, 1) == 0, "F_UNLCK");
    int status;
    waitpid(child, &status, 0);
    // Closing any descriptor for the file releases the process's locks,
    // but not the open file description's.
    int other = open(path, O_RDONLY);
    close(other);
    child = fork();
    if (child == 0) {
        int own = open(path, O_RDWR);
        printf("lock: after the parent closed a descriptor: %s\n", setlk(own, F_SETLK, F_WRLCK, 0, 10) == 0 ? "taken" : "refused");
        printf("lock: the description's still held: %s\n", setlk(own, F_SETLK, F_WRLCK, 100, 1) == 0 ? "no" : "yes");
        _exit(0);
    }
    waitpid(child, &status, 0);
    close(whole);
    close(fd);
    unlink(path);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    watching();
    memfd_and_mapping();
    shared_across_fork();
    passing_descriptors();
    path_sockets();
    locking();
    puts("done");
    return 0;
}
