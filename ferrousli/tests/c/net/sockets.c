/*
 * Sockets: a connected pair, a descriptor passed with SCM_RIGHTS through
 * headers whose padding holds junk, a listening socket in the test's
 * directory with the names of both ends, options, shutdown, and datagrams
 * with the sender's address.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>

#include "check.h"

static socklen_t unix_address(struct sockaddr_un *addr, const char *path)
{
	memset(addr, 0, sizeof *addr);
	addr->sun_family = AF_UNIX;
	strcpy(addr->sun_path, path);
	return sizeof *addr;
}

int main(void)
{
	int pair[2], file, passed, listener, client, server, a, b, type;
	char buf[64], payload[] = "x";
	char control[CMSG_SPACE(sizeof(int))];
	struct msghdr m;
	struct iovec iov;
	struct cmsghdr *c;
	struct sockaddr_un addr, name;
	socklen_t len;
	struct timeval tv;

	/* A pair, with send and recv. */
	CHECK(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, pair) == 0);
	CHECK(fcntl(pair[0], F_GETFD) & FD_CLOEXEC);
	CHECK(send(pair[0], "ping", 4, 0) == 4);
	CHECK(recv(pair[1], buf, sizeof buf, 0) == 4 && !memcmp(buf, "ping", 4));

	/* A descriptor passed with SCM_RIGHTS. Every padding byte of both
	 * headers is junk, which the kernel would read as part of a length. */
	file = open("passed", O_RDWR | O_CREAT, 0600);
	CHECK(file >= 0 && write(file, "abc", 3) == 3);
	memset(&m, 0xff, sizeof m);
	memset(control, 0xff, sizeof control);
	iov.iov_base = payload;
	iov.iov_len = 1;
	m.msg_name = 0;
	m.msg_namelen = 0;
	m.msg_iov = &iov;
	m.msg_iovlen = 1;
	m.msg_control = control;
	m.msg_controllen = sizeof control;
	m.msg_flags = 0;
	c = CMSG_FIRSTHDR(&m);
	c->cmsg_len = CMSG_LEN(sizeof(int));
	c->cmsg_level = SOL_SOCKET;
	c->cmsg_type = SCM_RIGHTS;
	memcpy(CMSG_DATA(c), &file, sizeof file);
	CHECK(sendmsg(pair[0], &m, 0) == 1);
	close(file);

	memset(&m, 0xff, sizeof m);
	memset(control, 0, sizeof control);
	iov.iov_base = buf;
	iov.iov_len = sizeof buf;
	m.msg_name = 0;
	m.msg_namelen = 0;
	m.msg_iov = &iov;
	m.msg_iovlen = 1;
	m.msg_control = control;
	m.msg_controllen = sizeof control;
	m.msg_flags = 0;
	CHECK(recvmsg(pair[1], &m, 0) == 1 && buf[0] == 'x');
	c = CMSG_FIRSTHDR(&m);
	CHECK(c && c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS);
	passed = -1;
	if (c)
		memcpy(&passed, CMSG_DATA(c), sizeof passed);
	CHECK(passed >= 0 && pread(passed, buf, 3, 0) == 3 && !memcmp(buf, "abc", 3));

	/* A listening socket, two clients, accept4 and accept, and the names of
	 * both ends. */
	listener = socket(AF_UNIX, SOCK_STREAM, 0);
	CHECK(listener >= 0);
	len = unix_address(&addr, "listener");
	CHECK(bind(listener, (struct sockaddr *)&addr, len) == 0);
	CHECK(listen(listener, 4) == 0);
	client = socket(AF_UNIX, SOCK_STREAM, 0);
	CHECK(connect(client, (struct sockaddr *)&addr, len) == 0);
	len = sizeof name;
	server = accept4(listener, (struct sockaddr *)&name, &len, SOCK_CLOEXEC);
	CHECK(server >= 0 && (fcntl(server, F_GETFD) & FD_CLOEXEC));
	b = socket(AF_UNIX, SOCK_STREAM, 0);
	CHECK(connect(b, (struct sockaddr *)&addr, sizeof addr) == 0);
	a = accept(listener, 0, 0);
	CHECK(a >= 0 && !(fcntl(a, F_GETFD) & FD_CLOEXEC));
	close(a);
	close(b);
	len = sizeof name;
	CHECK(getsockname(listener, (struct sockaddr *)&name, &len) == 0);
	CHECK(!strcmp(name.sun_path, "listener"));
	len = sizeof name;
	CHECK(getpeername(client, (struct sockaddr *)&name, &len) == 0);
	CHECK(!strcmp(name.sun_path, "listener"));

	/* Options, with a receive timeout read back as it was set. */
	len = sizeof type;
	CHECK(getsockopt(client, SOL_SOCKET, SO_TYPE, &type, &len) == 0);
	CHECK(type == SOCK_STREAM && len == sizeof type);
	tv.tv_sec = 2;
	tv.tv_usec = 500000;
	CHECK(setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv) == 0);
	memset(&tv, 0, sizeof tv);
	len = sizeof tv;
	CHECK(getsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &tv, &len) == 0);
	CHECK(tv.tv_sec == 2 && tv.tv_usec == 500000);

	/* Shutting down writing: the other end reads the end of the stream. */
	CHECK(shutdown(client, SHUT_WR) == 0);
	CHECK(recv(server, buf, sizeof buf, 0) == 0);

	/* Datagrams, with the sender's address. */
	a = socket(AF_UNIX, SOCK_DGRAM, 0);
	b = socket(AF_UNIX, SOCK_DGRAM, 0);
	len = unix_address(&name, "sender");
	CHECK(bind(a, (struct sockaddr *)&name, len) == 0);
	len = unix_address(&addr, "receiver");
	CHECK(bind(b, (struct sockaddr *)&addr, len) == 0);
	CHECK(sendto(a, "hi", 2, 0, (struct sockaddr *)&addr, len) == 2);
	memset(&name, 0, sizeof name);
	len = sizeof name;
	CHECK(recvfrom(b, buf, sizeof buf, 0, (struct sockaddr *)&name, &len) == 2);
	CHECK(!memcmp(buf, "hi", 2) && !strcmp(name.sun_path, "sender"));

	/* Errors come back through errno. */
	errno = 0;
	CHECK(socket(AF_UNIX, 12345, 0) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(listen(-1, 1) == -1 && errno == EBADF);
	return t_status;
}
