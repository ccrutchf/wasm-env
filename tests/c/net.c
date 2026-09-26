// Sockets over loopback (no network needed): TCP connect/accept with a
// non-blocking connect finished by poll and SO_ERROR, send/recv, UDP
// sendto/recvfrom, select, getsockname, and a refused connection.
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <unistd.h>

static struct sockaddr_in loopback(int port)
{
	struct sockaddr_in a = { .sin_family = AF_INET, .sin_port = htons(port) };
	a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	return a;
}

int main(void)
{
	// TCP: a listener on an ephemeral port.
	int srv = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	int one = 1;
	setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
	struct sockaddr_in a = loopback(0);
	if (bind(srv, (struct sockaddr *)&a, sizeof a) || listen(srv, 4)) {
		perror("bind/listen");
		return 1;
	}
	socklen_t len = sizeof a;
	getsockname(srv, (struct sockaddr *)&a, &len);
	printf("listening: %s, port assigned: %s\n", inet_ntoa(a.sin_addr), ntohs(a.sin_port) ? "yes" : "no");

	// Non-blocking connect: EINPROGRESS (or done at once), then poll for
	// writability and read SO_ERROR.
	int c = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
	int r = connect(c, (struct sockaddr *)&a, sizeof a);
	printf("connect: %s\n", r == 0 ? "done" : errno == EINPROGRESS ? "in progress" : strerror(errno));
	struct pollfd p = { .fd = c, .events = POLLOUT };
	printf("poll: %d, writable %d\n", poll(&p, 1, 5000), !!(p.revents & POLLOUT));
	int err = -1;
	len = sizeof err;
	getsockopt(c, SOL_SOCKET, SO_ERROR, &err, &len);
	printf("SO_ERROR %d\n", err);
	setsockopt(c, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
	fcntl(c, F_SETFL, fcntl(c, F_GETFL) & ~O_NONBLOCK);

	int s = accept4(srv, 0, 0, SOCK_CLOEXEC);
	printf("accepted: %s\n", s >= 0 ? "yes" : strerror(errno));
	const char *msg = "hello over tcp";
	printf("sent %zd\n", write(c, msg, strlen(msg)));
	char buf[64] = { 0 };
	ssize_t n = read(s, buf, sizeof buf - 1);
	printf("received %zd: %s\n", n, buf);
	printf("sent back %zd\n", send(s, "pong", 4, MSG_NOSIGNAL));
	fd_set rd;
	FD_ZERO(&rd);
	FD_SET(c, &rd);
	struct timeval tv = { 5, 0 };
	printf("select: %d, readable %d\n", select(c + 1, &rd, 0, 0, &tv), FD_ISSET(c, &rd));
	memset(buf, 0, sizeof buf);
	n = recv(c, buf, sizeof buf - 1, 0);
	printf("received %zd: %s\n", n, buf);
	close(c);
	n = read(s, buf, sizeof buf);
	printf("after close: read %zd\n", n);
	close(s);

	// Nothing ready: poll times out.
	p = (struct pollfd){ .fd = srv, .events = POLLIN };
	printf("idle poll: %d\n", poll(&p, 1, 50));

	// UDP.
	int u1 = socket(AF_INET, SOCK_DGRAM, 0), u2 = socket(AF_INET, SOCK_DGRAM, 0);
	struct sockaddr_in b = loopback(0);
	bind(u1, (struct sockaddr *)&b, sizeof b);
	len = sizeof b;
	getsockname(u1, (struct sockaddr *)&b, &len);
	printf("udp sent %zd\n", sendto(u2, "datagram", 8, 0, (struct sockaddr *)&b, sizeof b));
	struct sockaddr_in from;
	len = sizeof from;
	memset(buf, 0, sizeof buf);
	n = recvfrom(u1, buf, sizeof buf, 0, (struct sockaddr *)&from, &len);
	printf("udp received %zd: %s from %s\n", n, buf, inet_ntoa(from.sin_addr));

	// A port nobody listens on (the listener's, once closed).
	close(srv);
	int r2 = socket(AF_INET, SOCK_STREAM, 0);
	r = connect(r2, (struct sockaddr *)&a, sizeof a);
	printf("refused: %s\n", r < 0 && errno == ECONNREFUSED ? "ECONNREFUSED" : "unexpected");

	printf("unix socket: %s\n", socket(AF_UNIX, SOCK_STREAM, 0) < 0 && errno == EAFNOSUPPORT ? "EAFNOSUPPORT" : "?");
	return 0;
}
