// wasm-env: posix_spawn built on vfork (see vfork.c). This follows musl's
// src/process/posix_spawn.c, except that the child shares memory with the
// parent, so it reports exec errors through a shared variable instead of
// a pipe. Signal handlers can't run in the child (the runtime doesn't
// deliver to handlers yet), so there's no handler reset.
#define _GNU_SOURCE
#include <spawn.h>
#include <fcntl.h>
#include <unistd.h>
#include <signal.h>
#include <errno.h>
#include <sys/wait.h>
#include "syscall.h"
#include "../fdop.h"

static int child(const posix_spawn_file_actions_t *fa, const posix_spawnattr_t *attr,
                 const char *path, char *const argv[], char *const envp[])
{
	int ret;

	if (attr->__flags & POSIX_SPAWN_SETSID)
		if ((ret=__syscall(SYS_setsid)) < 0)
			return ret;

	if (attr->__flags & POSIX_SPAWN_SETPGROUP)
		if ((ret=__syscall(SYS_setpgid, 0, attr->__pgrp)))
			return ret;

	if (fa && fa->__actions) {
		struct fdop *op;
		int fd;
		for (op = fa->__actions; op->next; op = op->next);
		for (; op; op = op->prev) {
			switch(op->cmd) {
			case FDOP_CLOSE:
				__syscall(SYS_close, op->fd);
				break;
			case FDOP_DUP2:
				fd = op->srcfd;
				if (fd != op->fd) {
					if ((ret=__syscall(SYS_dup3, fd, op->fd, 0))<0)
						return ret;
				} else {
					ret = __syscall(SYS_fcntl, fd, F_GETFD);
					ret = __syscall(SYS_fcntl, fd, F_SETFD,
					                ret & ~FD_CLOEXEC);
					if (ret<0)
						return ret;
				}
				break;
			case FDOP_OPEN:
				fd = __syscall(SYS_openat, AT_FDCWD, op->path, op->oflag, op->mode);
				if ((ret=fd) < 0) return ret;
				if (fd != op->fd) {
					if ((ret=__syscall(SYS_dup3, fd, op->fd, 0))<0)
						return ret;
					__syscall(SYS_close, fd);
				}
				break;
			case FDOP_CHDIR:
				ret = __syscall(SYS_chdir, op->path);
				if (ret<0) return ret;
				break;
			case FDOP_FCHDIR:
				ret = __syscall(SYS_fchdir, op->fd);
				if (ret<0) return ret;
				break;
			}
		}
	}

	if (attr->__flags & POSIX_SPAWN_SETSIGMASK)
		__syscall(SYS_rt_sigprocmask, SIG_SETMASK, &attr->__mask, 0, _NSIG/8);

	int (*exec)(const char *, char *const *, char *const *) =
		attr->__fn ? (int (*)())attr->__fn : execve;
	exec(path, argv, envp);
	return -errno;
}

int posix_spawn(pid_t *restrict res, const char *restrict path,
	const posix_spawn_file_actions_t *fa,
	const posix_spawnattr_t *restrict attr,
	char *const argv[restrict], char *const envp[restrict])
{
	static const posix_spawnattr_t no_attr;
	// Shared with the child, which runs in our memory until it execs.
	volatile int err = 0;
	if (!attr) attr = &no_attr;

	pid_t pid = vfork();
	if (pid == 0) {
		err = -child(fa, attr, path, argv, envp);
		_exit(127);
	}
	if (pid < 0) return errno;
	if (err) {
		waitpid(pid, 0, 0);
		return err;
	}
	if (res) *res = pid;
	return 0;
}
