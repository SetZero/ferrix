/*
 * Adapted from libc-test's src/regression/ftello-unflushed-append.c (MIT).
 * The file is made with open(O_EXCL) rather than mkstemp.
 * commit 3af2edee150484940916eba1984f78c3b965dd05 2014-02-07
 * fix ftello result for append streams with unflushed output
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "test.h"

#define ASSERT(c) do { \
	errno = 0; \
	if (!(c)) \
		t_error("%s failed (errno: %s)\n", #c, strerror(errno)); \
} while(0)

int main(void)
{
	char tmp[] = "ftello.tmp";
	int fd;
	FILE *f;
	off_t off;

	ASSERT((fd = open(tmp, O_RDWR | O_CREAT | O_EXCL, 0600)) > 2);
	ASSERT(write(fd, "abcd", 4) == 4);
	ASSERT(close(fd) == 0);

	ASSERT((fd = open(tmp, O_WRONLY)) > 2);
	ASSERT(f = fdopen(fd, "a"));
	if (f) {
		ASSERT(fwrite("efg", 1, 3, f) == 3);
		ASSERT((off = ftello(f)) != -1);
		if (off != 7)
			t_error("ftello is broken before flush: got %lld, want 7\n", (long long)off);
		ASSERT(fflush(f) == 0);
		ASSERT((off = ftello(f)) != -1);
		if (off != 7)
			t_error("ftello is broken after flush: got %lld, want 7\n", (long long)off);
		ASSERT(fclose(f) == 0);
	}
	if (fd > 2)
		ASSERT(unlink(tmp) == 0);
	return t_status;
}
