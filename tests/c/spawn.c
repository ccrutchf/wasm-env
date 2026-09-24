// M1: process creation from C. Expects /bin/busybox and /bin/sh.
#define _GNU_SOURCE
#include <errno.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

int main(void)
{
	// posix_spawn with stdout redirected into a pipe.
	int p[2];
	pipe(p);
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, p[1], 1);
	posix_spawn_file_actions_addclose(&fa, p[0]);
	char *argv[] = {"echo", "spawned", NULL};
	pid_t pid;
	int err = posix_spawn(&pid, "/bin/busybox", &fa, NULL, argv, environ);
	close(p[1]);
	char buf[64] = {0};
	read(p[0], buf, sizeof buf - 1);
	int status;
	waitpid(pid, &status, 0);
	printf("posix_spawn: err=%d output=%s", err, buf);
	printf("posix_spawn: exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));

	// posix_spawnp searches PATH; a missing program reports ENOENT.
	char *missing[] = {"no-such-program", NULL};
	err = posix_spawnp(&pid, "no-such-program", NULL, NULL, missing, environ);
	printf("posix_spawnp missing: %s\n", err == ENOENT ? "ENOENT" : strerror(err));

	// Raw vfork: the child's _exit status reaches the parent.
	pid = vfork();
	if (pid == 0) _exit(42);
	waitpid(pid, &status, 0);
	printf("vfork: child exit %d\n", WEXITSTATUS(status));

	// system() and popen() go through /bin/sh.
	int rc = system("exit 5");
	printf("system: %d\n", WEXITSTATUS(rc));
	FILE *f = popen("echo from popen | tr a-z A-Z", "r");
	fgets(buf, sizeof buf, f);
	printf("popen: %s", buf);
	printf("pclose: %d\n", pclose(f));

	// A child killed by a signal.
	pid = vfork();
	if (pid == 0) {
		char *sleeper[] = {"sleep", "10", NULL};
		execve("/bin/sleep", sleeper, environ);
		_exit(127);
	}
	kill(pid, SIGTERM);
	waitpid(pid, &status, 0);
	printf("kill: signaled=%d sig=%d\n", WIFSIGNALED(status), WTERMSIG(status));

	printf("getpid=%d getppid=%d\n", getpid(), getppid());
	return 0;
}
