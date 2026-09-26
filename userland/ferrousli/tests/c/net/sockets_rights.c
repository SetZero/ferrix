/*
 * A descriptor passed over a Unix socket pair with SCM_RIGHTS. Every padding
 * byte of msghdr and cmsghdr is junk, which a kernel with size_t lengths
 * would read as the high half of a length if the C library passed it on.
 *
 * It is the third of three programs, one for each part of a kernel's Unix
 * sockets: sockets_pair.c, sockets_names.c, then this one.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	int pair[2], file, passed;
	char buf[64], payload[] = "x";
	char control[CMSG_SPACE(sizeof(int))];
	struct msghdr m;
	struct iovec iov;
	struct cmsghdr *c;

	CHECK(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, pair) == 0);
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

	close(passed);
	close(pair[0]);
	close(pair[1]);
	return t_status;
}
