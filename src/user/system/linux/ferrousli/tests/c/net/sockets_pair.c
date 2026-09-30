/*
 * Connected pairs of Unix sockets, with no names: a stream pair with send and
 * recv, a sequenced-packet and a datagram pair that keep message boundaries,
 * SO_TYPE, a receive timeout read back as it was set, shutdown giving the
 * peer the end of the stream, and errors through errno.
 *
 * It is the first of three programs, one for each part of a kernel's Unix
 * sockets: this one, then sockets_names.c and sockets_rights.c.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <fcntl.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#include "check.h"

/* Sends two messages on a pair of `type` and checks they arrive apart. */
static void boundaries(int type)
{
	int pair[2], got;
	char buf[64];

	CHECK(socketpair(AF_UNIX, type, 0, pair) == 0);
	CHECK(send(pair[0], "ab", 2, 0) == 2 && send(pair[0], "cde", 3, 0) == 3);
	got = recv(pair[1], buf, sizeof buf, 0);
	CHECK(got == 2 && !memcmp(buf, "ab", 2));
	got = recv(pair[1], buf, sizeof buf, 0);
	CHECK(got == 3 && !memcmp(buf, "cde", 3));
	close(pair[0]);
	close(pair[1]);
}

int main(void)
{
	int pair[2], type;
	char buf[64];
	socklen_t len;
	struct timeval tv;

	/* A stream pair, close-on-exec, with send and recv both ways. */
	CHECK(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, pair) == 0);
	CHECK(fcntl(pair[0], F_GETFD) & FD_CLOEXEC);
	CHECK(fcntl(pair[1], F_GETFD) & FD_CLOEXEC);
	CHECK(send(pair[0], "ping", 4, 0) == 4);
	CHECK(recv(pair[1], buf, sizeof buf, 0) == 4 && !memcmp(buf, "ping", 4));
	CHECK(send(pair[1], "pong", 4, 0) == 4);
	CHECK(recv(pair[0], buf, sizeof buf, 0) == 4 && !memcmp(buf, "pong", 4));

	/* Options, with a receive timeout read back as it was set. */
	len = sizeof type;
	CHECK(getsockopt(pair[0], SOL_SOCKET, SO_TYPE, &type, &len) == 0);
	CHECK(type == SOCK_STREAM && len == sizeof type);
	tv.tv_sec = 2;
	tv.tv_usec = 500000;
	CHECK(setsockopt(pair[0], SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv) == 0);
#if !defined(FERROUSLI_TEST_EMULATED) || LONG_MAX > 0x7fffffff
	/* User-mode QEMU refuses to set a 32-bit target's SO_RCVTIMEO_NEW, so
	 * the old option is set in its place, but "reads" the new one as a
	 * zero int. */
	memset(&tv, 0, sizeof tv);
	len = sizeof tv;
	CHECK(getsockopt(pair[0], SOL_SOCKET, SO_RCVTIMEO, &tv, &len) == 0);
	CHECK(tv.tv_sec == 2 && tv.tv_usec == 500000);
#endif

	/* Shutting down writing: the other end reads the end of the stream. */
	CHECK(shutdown(pair[0], SHUT_WR) == 0);
	CHECK(recv(pair[1], buf, sizeof buf, 0) == 0);
	close(pair[0]);
	close(pair[1]);

	/* Packets and datagrams keep their boundaries. */
	boundaries(SOCK_SEQPACKET);
	boundaries(SOCK_DGRAM);

	/* Errors come back through errno. */
	errno = 0;
#ifndef FERROUSLI_TEST_EMULATED
	/* qemu-user passes an unknown type on in a form the kernel accepts. */
	CHECK(socket(AF_UNIX, 12345, 0) == -1 && errno == EINVAL);
#endif
	errno = 0;
	CHECK(listen(-1, 1) == -1 && errno == EBADF);
	return t_status;
}
