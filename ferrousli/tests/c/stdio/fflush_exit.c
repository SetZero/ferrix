/*
 * Adapted from libc-test's src/regression/fflush-exit.c (MIT). The file is
 * made with open(O_EXCL) rather than mkstemp.
 *
 * gcc 4.9.0 introduced an invalid optimization for local weak alias symbols
 * which drops stdout fflush from exit in musl
 * https://gcc.gnu.org/bugzilla/show_bug.cgi?id=61144
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include "test.h"

#define ASSERT(c) do { \
	errno = 0; \
	if (!(c)) \
		t_error("%s failed (errno: %s)\n", #c, strerror(errno)); \
} while(0)

int main(void)
{
	char tmp[] = "fflush-exit.tmp";
	int fd, pid, status;
	char c;

	ASSERT((fd = open(tmp, O_RDWR | O_CREAT | O_EXCL, 0600)) > 2);
	ASSERT((pid = fork()) >= 0);
	if (pid == 0) {
		ASSERT(close(1) == 0);
		ASSERT(dup(fd) == 1);
		ASSERT(fwrite("x", 1, 1, stdout) == 1);
		exit(t_status);
	}
	ASSERT(waitpid(pid, &status, 0) == pid);
	ASSERT(WIFEXITED(status) && WEXITSTATUS(status) == 0);
	ASSERT(pread(fd, &c, 1, 0) == 1);
	ASSERT(c == 'x');
	ASSERT(unlink(tmp) == 0);
	return t_status;
}
