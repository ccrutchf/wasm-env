// Signal handlers: delivery with siginfo and ucontext, masks and pending
// signals, SA_RESETHAND, an alternate stack, sigsuspend, siglongjmp out of
// a handler, a handler interrupting a blocking read (EINTR), SIGCHLD when
// a forked child exits, and registers surviving a handler. Output doesn't
// depend on timing.
#define _GNU_SOURCE
#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

static volatile sig_atomic_t hits, last_sig, info_ok, on_alt;
static char alt[64 * 1024];

static void handler(int sig, siginfo_t *info, void *uc)
{
	hits++;
	last_sig = sig;
	info_ok = info->si_signo == sig && uc != NULL;
	char here;
	on_alt = &here >= alt && &here < alt + sizeof alt;
}

static void on(int sig, int flags)
{
	struct sigaction sa = { 0 };
	sa.sa_sigaction = handler;
	sa.sa_flags = SA_SIGINFO | flags;
	sigemptyset(&sa.sa_mask);
	sigaction(sig, &sa, NULL);
}

static sigjmp_buf jump;
static void jumper(int sig) { siglongjmp(jump, sig); }

static volatile sig_atomic_t children;
static void child_exited(int sig) { (void)sig; children++; }

int main(void)
{
	on(SIGUSR1, 0);
	raise(SIGUSR1);
	printf("raise: %d hit(s), signal %d, siginfo %s\n", hits, last_sig, info_ok ? "ok" : "wrong");

	// Blocked, it stays pending; unblocked, it's delivered.
	sigset_t set, old;
	sigemptyset(&set);
	sigaddset(&set, SIGUSR1);
	sigprocmask(SIG_BLOCK, &set, &old);
	raise(SIGUSR1);
	sigset_t pending;
	sigpending(&pending);
	printf("blocked: %d hit(s), pending %d\n", hits, sigismember(&pending, SIGUSR1));
	sigprocmask(SIG_SETMASK, &old, NULL);
	printf("unblocked: %d hit(s)\n", hits);

	// SA_RESETHAND: once, then the default action (ignore, for SIGCHLD-like
	// SIGURG).
	on(SIGURG, SA_RESETHAND);
	raise(SIGURG);
	raise(SIGURG);
	printf("resethand: %d hit(s)\n", hits);

	// An alternate stack.
	stack_t ss = { .ss_sp = alt, .ss_size = sizeof alt, .ss_flags = 0 };
	sigaltstack(&ss, NULL);
	on(SIGUSR2, SA_ONSTACK);
	raise(SIGUSR2);
	printf("sigaltstack: ran on it: %s\n", on_alt ? "yes" : "no");

	// siglongjmp out of a handler, restoring the mask.
	signal(SIGTERM, jumper);
	int got = sigsetjmp(jump, 1);
	if (got == 0) {
		kill(getpid(), SIGTERM);
		puts("not reached");
	}
	sigprocmask(SIG_SETMASK, NULL, &old);
	printf("siglongjmp: from signal %d, SIGTERM blocked after: %d\n", got, sigismember(&old, SIGTERM));

	// sigsuspend: wait for a signal with it unblocked just for the wait.
	sigprocmask(SIG_BLOCK, &set, &old);
	raise(SIGUSR1);
	sigset_t wait_mask;
	sigemptyset(&wait_mask);
	int before = hits;
	sigsuspend(&wait_mask);
	sigprocmask(SIG_SETMASK, NULL, &set);
	printf("sigsuspend: %d hit(s) during, SIGUSR1 blocked again: %d\n", hits - before, sigismember(&set, SIGUSR1));
	sigprocmask(SIG_SETMASK, &old, NULL);

	// A signal interrupts a blocking read (no SA_RESTART): EINTR.
	int fds[2];
	pipe(fds);
	pid_t child = fork();
	if (child == 0) {
		usleep(50000);
		kill(getppid(), SIGUSR1);
		usleep(50000);
		_exit(0);
	}
	signal(SIGCHLD, child_exited);
	char c;
	ssize_t n = read(fds[0], &c, 1);
	printf("read interrupted: %s\n", n < 0 && errno == EINTR ? "EINTR" : "no");

	// SIGCHLD when the child exits.
	int status;
	while (waitpid(child, &status, 0) < 0 && errno == EINTR)
		;
	for (int i = 0; i < 100 && !children; i++)
		usleep(1000);
	printf("SIGCHLD: %d\n", children);

	// Values in callee-saved registers survive a handler.
	register long keep asm("x19") = 0x123456789;
	raise(SIGUSR1);
	asm volatile("" : "+r"(keep));
	printf("x19 kept: %s\n", keep == 0x123456789 ? "yes" : "no");
	return 0;
}
