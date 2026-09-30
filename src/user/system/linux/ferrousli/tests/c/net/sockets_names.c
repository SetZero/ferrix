/*
 * Unix sockets with names in the test's directory: a listening stream socket,
 * two clients taken with accept4 and accept, the names of both ends, and
 * datagrams with the sender's name.
 *
 * It is the second of three programs, one for each part of a kernel's Unix
 * sockets: sockets_pair.c, this one, then sockets_rights.c. Every address
 * length it passes is the whole struct sockaddr_un.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/socket.h>
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
	int listener, client, server, other, taken, a, b;
	char buf[64];
	struct sockaddr_un addr, name;
	socklen_t len;

	/* A listening socket, a client, accept4, and the names of both ends. */
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
	len = sizeof name;
	CHECK(getsockname(listener, (struct sockaddr *)&name, &len) == 0);
	CHECK(!strcmp(name.sun_path, "listener"));
	len = sizeof name;
	CHECK(getpeername(client, (struct sockaddr *)&name, &len) == 0);
	CHECK(!strcmp(name.sun_path, "listener"));

	/* The connection carries bytes both ways. */
	CHECK(send(client, "hello", 5, 0) == 5);
	CHECK(recv(server, buf, sizeof buf, 0) == 5 && !memcmp(buf, "hello", 5));
	CHECK(send(server, "back", 4, 0) == 4);
	CHECK(recv(client, buf, sizeof buf, 0) == 4 && !memcmp(buf, "back", 4));

	/* A second client, taken with plain accept, not close-on-exec. */
	other = socket(AF_UNIX, SOCK_STREAM, 0);
	CHECK(connect(other, (struct sockaddr *)&addr, sizeof addr) == 0);
	taken = accept(listener, 0, 0);
	CHECK(taken >= 0 && !(fcntl(taken, F_GETFD) & FD_CLOEXEC));

	/* Datagrams, with the sender's name. */
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

	/* A name already bound is refused. */
	len = unix_address(&addr, "listener");
	errno = 0;
	CHECK(bind(a, (struct sockaddr *)&addr, len) == -1 && (errno == EADDRINUSE || errno == EINVAL));

	close(listener);
	close(client);
	close(server);
	close(other);
	close(taken);
	close(a);
	close(b);
	return t_status;
}
