/*
 * Pipes and descriptors: close-on-exec, duplication, descriptor flags,
 * scatter-gather I/O, poll and select, and dup2 onto standard output.
 * Prints "to stdout" once, after putting standard output back.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/select.h>
#include <sys/uio.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	int p[2], q[2], saved, flags, d, file, n;
	char buf[16], a[2], b[8];
	struct iovec out[2], in[2];
	struct pollfd pfd[2], bad;
	struct timespec zero = { 0, 0 };
	struct timeval tv;
	struct winsize ws;
	fd_set r, w;

	/* pipe2 with O_CLOEXEC, checked through F_GETFD. */
	CHECK(pipe2(p, O_CLOEXEC) == 0);
	CHECK(fcntl(p[0], F_GETFD) == FD_CLOEXEC && fcntl(p[1], F_GETFD) == FD_CLOEXEC);
	CHECK(pipe(q) == 0);
	CHECK(fcntl(q[0], F_GETFD) == 0 && fcntl(q[1], F_GETFD) == 0);
	CHECK(fcntl(q[0], F_SETFD, FD_CLOEXEC) == 0 && fcntl(q[0], F_GETFD) == FD_CLOEXEC);
	errno = 0;
	CHECK(pipe2(q, 0x40000000) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(fcntl(-1, F_GETFD) == -1 && errno == EBADF);

	/* O_NONBLOCK through F_SETFL: an empty pipe then reads EAGAIN. */
	flags = fcntl(p[0], F_GETFL);
	CHECK(flags >= 0 && !(flags & O_NONBLOCK) && (flags & O_ACCMODE) == O_RDONLY);
	CHECK(fcntl(p[0], F_SETFL, flags | O_NONBLOCK) == 0);
	CHECK(fcntl(p[0], F_GETFL) & O_NONBLOCK);
	errno = 0;
	CHECK(read(p[0], buf, 1) == -1 && errno == EAGAIN);

	/* dup and F_DUPFD_CLOEXEC: the copy shares the pipe, not the flag. */
	d = dup(p[1]);
	CHECK(d >= 0 && fcntl(d, F_GETFD) == 0);
	CHECK(write(d, "ab", 2) == 2 && read(p[0], buf, 2) == 2 && memcmp(buf, "ab", 2) == 0);
	CHECK(close(d) == 0);
	d = fcntl(p[1], F_DUPFD_CLOEXEC, 100);
	CHECK(d >= 100 && fcntl(d, F_GETFD) == FD_CLOEXEC);
	CHECK(close(d) == 0);
	errno = 0;
	CHECK(dup(-1) == -1 && errno == EBADF);

	/* dup3 and dup2, and equal descriptors. */
	CHECK(dup3(p[1], 50, O_CLOEXEC) == 50 && fcntl(50, F_GETFD) == FD_CLOEXEC);
	errno = 0;
	CHECK(dup3(50, 50, 0) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(dup3(p[1], 51, 0x40000000) == -1 && errno == EINVAL);
	CHECK(dup2(50, 50) == 50);
	CHECK(dup2(q[1], 50) == 50 && fcntl(50, F_GETFD) == 0);
	CHECK(close(50) == 0);
	errno = 0;
	CHECK(dup2(50, 50) == -1 && errno == EBADF);
	errno = 0;
	CHECK(dup2(50, 51) == -1 && errno == EBADF);

	/* A pipe is not a terminal; a closed descriptor is not open. */
	errno = 0;
	CHECK(isatty(p[0]) == 0 && errno == ENOTTY);
	errno = 0;
	CHECK(isatty(-1) == 0 && errno == EBADF);

	/* ioctl passes its argument through. */
	CHECK(write(q[1], "abc", 3) == 3);
	CHECK(ioctl(q[0], FIONREAD, &n) == 0 && n == 3);
	CHECK(read(q[0], buf, 3) == 3);
	errno = 0;
	CHECK(ioctl(q[0], TIOCGWINSZ, &ws) == -1 && errno == ENOTTY);
	CHECK(fcntl(p[0], F_GETOWN) == 0);

	/* writev and readv. */
	out[0].iov_base = "hel";
	out[0].iov_len = 3;
	out[1].iov_base = "lo";
	out[1].iov_len = 2;
	in[0].iov_base = a;
	in[0].iov_len = sizeof a;
	in[1].iov_base = b;
	in[1].iov_len = sizeof b;
	CHECK(writev(p[1], out, 2) == 5);
	CHECK(readv(p[0], in, 2) == 5 && memcmp(a, "he", 2) == 0 && memcmp(b, "llo", 3) == 0);
	errno = 0;
	CHECK(writev(p[1], out, -1) == -1 && errno == EINVAL);

	/* pwritev and preadv leave the offset alone; a pipe cannot seek. */
	file = open("iov", O_RDWR | O_CREAT | O_EXCL, 0600);
	CHECK(file >= 0);
	CHECK(pwritev(file, out, 2, 10) == 5 && lseek(file, 0, SEEK_CUR) == 0);
	memset(a, 0, sizeof a);
	memset(b, 0, sizeof b);
	CHECK(preadv(file, in, 2, 10) == 5 && memcmp(a, "he", 2) == 0 && memcmp(b, "llo", 3) == 0);
	CHECK(preadv(file, in, 2, 0) == 10 && a[0] == 0);
	errno = 0;
	CHECK(preadv(p[0], in, 2, 0) == -1 && errno == ESPIPE);

	/* poll: the write end is ready, the empty read end is not. */
	pfd[0].fd = p[0];
	pfd[0].events = POLLIN;
	pfd[1].fd = p[1];
	pfd[1].events = POLLOUT;
	CHECK(poll(pfd, 2, 0) == 1 && pfd[0].revents == 0 && (pfd[1].revents & POLLOUT));
	CHECK(poll(pfd, 1, 10) == 0);
	CHECK(write(p[1], "x", 1) == 1);
	CHECK(poll(pfd, 1, -1) == 1 && (pfd[0].revents & POLLIN));
	CHECK(ppoll(pfd, 2, &zero, 0) == 2);
	pfd[1].fd = -1;
	CHECK(ppoll(pfd, 2, 0, 0) == 1 && pfd[1].revents == 0);
	CHECK(zero.tv_sec == 0 && zero.tv_nsec == 0);
	bad.fd = 1000;
	bad.events = POLLIN;
	CHECK(poll(&bad, 1, 0) == 1 && bad.revents == POLLNVAL);
	CHECK(read(p[0], buf, 1) == 1);

	/* select: the same, and the time left written back. */
	FD_ZERO(&r);
	FD_SET(p[0], &r);
	FD_ZERO(&w);
	FD_SET(p[1], &w);
	tv.tv_sec = 0;
	tv.tv_usec = 20000;
	CHECK(select(p[1] + 1, &r, &w, 0, &tv) == 1);
	CHECK(!FD_ISSET(p[0], &r) && FD_ISSET(p[1], &w));
	FD_ZERO(&r);
	FD_SET(p[0], &r);
	tv.tv_sec = 0;
	tv.tv_usec = 20000;
	CHECK(select(p[0] + 1, &r, 0, 0, &tv) == 0);
	CHECK(tv.tv_sec == 0 && tv.tv_usec == 0 && !FD_ISSET(p[0], &r));
	tv.tv_sec = -1;
	errno = 0;
	CHECK(select(0, 0, 0, 0, &tv) == -1 && errno == EINVAL);
	FD_ZERO(&w);
	FD_SET(p[1], &w);
	CHECK(pselect(p[1] + 1, 0, &w, 0, &zero, 0) == 1 && FD_ISSET(p[1], &w));

	/* dup2 onto standard output, a write, and standard output back. */
	file = open("out", O_RDWR | O_CREAT | O_TRUNC, 0600);
	CHECK(file >= 0);
	saved = dup(1);
	CHECK(saved >= 0);
	CHECK(dup2(file, 1) == 1);
	CHECK(write(1, "to the file\n", 12) == 12);
	CHECK(dup2(saved, 1) == 1);
	CHECK(close(saved) == 0);
	CHECK(pread(file, buf, sizeof buf, 0) == 12 && memcmp(buf, "to the file\n", 12) == 0);
	CHECK(write(1, "to stdout\n", 10) == 10);

	return t_status;
}
