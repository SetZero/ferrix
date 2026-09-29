/*
 * Record locks through fcntl and struct flock, seen from a child.
 *
 * Adapted from libc-test's src/functional/fcntl.c (MIT). tmpfile() is
 * replaced by a file in the working directory, and the lock is released and
 * taken by a child at the end.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	struct flock fl = { 0 };
	int fd;
	pid_t pid;
	int status;

	fd = open("locked", O_RDWR | O_CREAT | O_EXCL, 0600);
	CHECK(fd >= 0);

	fl.l_type = F_WRLCK;
	fl.l_whence = SEEK_SET;
	fl.l_start = 0;
	fl.l_len = 0;
	CHECK(fcntl(fd, F_SETLK, &fl) == 0);

	pid = fork();
	if (!pid) {
		fl.l_type = F_RDLCK;
		_exit(fcntl(fd, F_SETLK, &fl) == 0 ||
		      (errno != EAGAIN && errno != EACCES));
	}
	while (waitpid(pid, &status, 0) < 0 && errno == EINTR)
		;
	CHECK(status == 0);

	pid = fork();
	if (!pid) {
		fl.l_type = F_WRLCK;
		_exit(fcntl(fd, F_GETLK, &fl) || fl.l_pid != getppid() ||
		      fl.l_type != F_WRLCK || fl.l_start != 0 || fl.l_len != 0);
	}
	while (waitpid(pid, &status, 0) < 0 && errno == EINTR)
		;
	CHECK(status == 0);

	/* Released, the lock can be taken by the child. */
	fl.l_type = F_UNLCK;
	CHECK(fcntl(fd, F_SETLK, &fl) == 0);
	pid = fork();
	if (!pid) {
		fl.l_type = F_WRLCK;
		_exit(fcntl(fd, F_SETLK, &fl) != 0);
	}
	while (waitpid(pid, &status, 0) < 0 && errno == EINTR)
		;
	CHECK(status == 0);

	/* With nobody holding it, F_GETLK says so. */
	fl.l_type = F_WRLCK;
	CHECK(fcntl(fd, F_GETLK, &fl) == 0 && fl.l_type == F_UNLCK);
	errno = 0;
	fl.l_type = 12345;
	CHECK(fcntl(fd, F_SETLK, &fl) == -1 && errno == EINVAL);

	CHECK(close(fd) == 0);
	return t_status;
}
