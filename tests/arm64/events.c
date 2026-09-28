// eventfd, epoll and socketpair, as an event loop (libuv's, Node.js's)
// uses them: counters, readiness of pipes, eventfds and sockets, one-shot
// interest, timeouts, and a thread waking the loop through an eventfd.
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <unistd.h>

static int efd;

static void *waker(void *arg)
{
	(void)arg;
	usleep(20000);
	uint64_t one = 1;
	write(efd, &one, 8);
	return 0;
}

// The events reported, as "tag:flags" in the order the tags were given.
static void report(const char *what, struct epoll_event *ev, int n)
{
	printf("%s: %d", what, n);
	for (int k = 0; k < n; k++)
		printf(" %c:%s%s%s", (char)ev[k].data.u64, ev[k].events & EPOLLIN ? "in" : "",
		       ev[k].events & EPOLLOUT ? "out" : "", ev[k].events & EPOLLHUP ? "hup" : "");
	printf("\n");
}

static int cmp(const void *a, const void *b)
{
	return (int)((const struct epoll_event *)a)->data.u64 - (int)((const struct epoll_event *)b)->data.u64;
}

int main(void)
{
	// eventfd: a counter.
	efd = eventfd(3, EFD_NONBLOCK | EFD_CLOEXEC);
	uint64_t v = 4;
	write(efd, &v, 8);
	read(efd, &v, 8);
	printf("eventfd: %llu", (unsigned long long)v);
	printf(" then %s\n", read(efd, &v, 8) < 0 && errno == EAGAIN ? "EAGAIN" : "data");
	int sem = eventfd(2, EFD_SEMAPHORE);
	read(sem, &v, 8);
	printf("semaphore: %llu", (unsigned long long)v);
	read(sem, &v, 8);
	printf(" %llu\n", (unsigned long long)v);

	// epoll over a pipe, the eventfd and a socket pair.
	int p[2], s[2];
	pipe(p);
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, s);
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event ev[8];
	struct epoll_event e = {.events = EPOLLIN, .data.u64 = 'p'};
	epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &e);
	e = (struct epoll_event){.events = EPOLLIN, .data.u64 = 'e'};
	epoll_ctl(ep, EPOLL_CTL_ADD, efd, &e);
	e = (struct epoll_event){.events = EPOLLIN | EPOLLOUT, .data.u64 = 's'};
	epoll_ctl(ep, EPOLL_CTL_ADD, s[0], &e);
	printf("add twice: %s\n", epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &e) < 0 && errno == EEXIST ? "EEXIST" : "ok");
	int n = epoll_wait(ep, ev, 8, 0);
	report("idle", ev, n);
	write(p[1], "x", 1);
	write(s[1], "hello", 5);
	n = epoll_wait(ep, ev, 8, 100);
	qsort(ev, n, sizeof ev[0], cmp);
	report("ready", ev, n);
	char buf[16];
	read(p[0], buf, 1);
	printf("socket: %zd bytes\n", read(s[0], buf, sizeof buf));

	// One-shot: reported once, until re-armed.
	e = (struct epoll_event){.events = EPOLLIN | EPOLLONESHOT, .data.u64 = 'p'};
	epoll_ctl(ep, EPOLL_CTL_MOD, p[0], &e);
	e = (struct epoll_event){.events = EPOLLIN, .data.u64 = 's'};
	epoll_ctl(ep, EPOLL_CTL_MOD, s[0], &e);
	write(p[1], "yz", 2);
	report("oneshot", ev, epoll_wait(ep, ev, 8, 100));
	report("oneshot again", ev, epoll_wait(ep, ev, 8, 0));
	epoll_ctl(ep, EPOLL_CTL_DEL, p[0], 0);

	// A thread wakes the loop through the eventfd.
	pthread_t t;
	pthread_create(&t, 0, waker, 0);
	n = epoll_wait(ep, ev, 8, 5000);
	report("woken", ev, n);
	pthread_join(t, 0);
	read(efd, &v, 8);

	// The other end closes: hang-up.
	close(s[1]);
	n = epoll_wait(ep, ev, 8, 100);
	report("closed", ev, n);
	printf("socket after close: %zd\n", read(s[0], buf, sizeof buf));
	return 0;
}
