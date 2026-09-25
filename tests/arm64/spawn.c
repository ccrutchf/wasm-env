// Creating processes from a translated program: vfork + execve, vfork +
// _exit, a failed execve in a vfork child, posix_spawn (glibc clones onto a
// new stack) and system(). The parent's registers and locals must survive
// each child.
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static void report(const char *what, pid_t pid)
{
	int status = 0;
	if (pid < 0) {
		printf("%s: failed\n", what);
		return;
	}
	if (waitpid(pid, &status, 0) != pid) {
		printf("%s: waitpid failed\n", what);
		return;
	}
	if (WIFEXITED(status))
		printf("%s: exit %d\n", what, WEXITSTATUS(status));
	else
		printf("%s: status %#x\n", what, status);
}

int main(int argc, char **argv)
{
	if (argc > 1 && strcmp(argv[1], "child") == 0) {
		printf("child says %s\n", argc > 2 ? argv[2] : "?");
		return 5;
	}
	volatile double kept = 2.5; // a live FP value across the vforks
	long sum = 0;
	for (int i = 0; i < 3; i++) {
		fflush(stdout);
		pid_t pid = vfork();
		if (pid == 0) {
			char n[2] = { (char)('0' + i), 0 };
			char *args[] = { argv[0], "child", n, NULL };
			execve(argv[0], args, environ);
			_exit(127);
		}
		sum += pid > 0;
		report("vfork+execve", pid);
	}
	pid_t pid = vfork();
	if (pid == 0)
		_exit(42);
	report("vfork+_exit", pid);
	pid = vfork();
	if (pid == 0) {
		char *args[] = { "/nonexistent", NULL };
		execve(args[0], args, environ);
		_exit(99); // execve failed: the child carries on to here
	}
	report("vfork+failed execve", pid);

	fflush(stdout);
	char *args[] = { argv[0], "child", "spawned", NULL };
	if (posix_spawn(&pid, argv[0], NULL, NULL, args, environ) != 0)
		pid = -1;
	report("posix_spawn", pid);
	fflush(stdout);
	int st = system("echo from system; exit 3");
	printf("system: exit %d\n", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
	printf("parent still has %g and %ld\n", kept, sum);
	return 0;
}
