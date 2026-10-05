// Between processes and about them: a named POSIX semaphore posted by a
// forked child (sem_open: a file in /dev/shm, mapped shared, whose futex the
// two processes share), CPU times (getrusage: a busy child's count, a
// sleeping one's barely do) and /proc/self/maps.
#include <fcntl.h>
#include <semaphore.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static double children_user(void)
{
	struct rusage r;
	getrusage(RUSAGE_CHILDREN, &r);
	return r.ru_utime.tv_sec + r.ru_utime.tv_usec / 1e6;
}

int main(void)
{
	char name[64];
	snprintf(name, sizeof name, "/wasm-env-test-%d", getpid());
	sem_t *s = sem_open(name, O_CREAT | O_EXCL, 0600, 0);
	if (s == SEM_FAILED) {
		perror("sem_open");
		return 1;
	}
	pid_t pid = fork();
	if (pid == 0) {
		usleep(200000);
		sem_post(s);
		_exit(0);
	}
	struct timespec deadline;
	clock_gettime(CLOCK_REALTIME, &deadline);
	deadline.tv_sec += 10;
	printf("posted by the child: %s\n", sem_timedwait(s, &deadline) == 0 ? "yes" : "no");
	waitpid(pid, NULL, 0);
	sem_close(s);
	sem_unlink(name);

	double before = children_user();
	if ((pid = fork()) == 0) {
		volatile unsigned long x = 0;
		for (unsigned long i = 0; i < 1000000000ul; i++)
			x += i;
		_exit(0);
	}
	waitpid(pid, NULL, 0);
	double busy = children_user() - before;
	before = children_user();
	if ((pid = fork()) == 0) {
		sleep(1);
		_exit(0);
	}
	waitpid(pid, NULL, 0);
	double idle = children_user() - before;
	printf("a busy child's CPU time counts: %s\n", busy > 0.1 ? "yes" : "no");
	printf("a sleeping child's barely does: %s\n", idle < 0.5 ? "yes" : "no");

	FILE *f = fopen("/proc/self/maps", "r");
	int stack = 0, libc = 0;
	char line[512];
	while (f && fgets(line, sizeof line, f)) {
		stack |= strstr(line, "[stack]") != NULL;
		libc |= strstr(line, "libc.so") != NULL;
	}
	printf("maps: stack %s, libc %s\n", stack ? "yes" : "no", libc ? "yes" : "no");
	return 0;
}
