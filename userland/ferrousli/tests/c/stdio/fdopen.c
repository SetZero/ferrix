/*
 * Adapted from libc-test's src/functional/fdopen.c (MIT). The file is made
 * with open(O_EXCL) in the test's own directory rather than mkstemp.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "test.h"

#define TEST(c) do { \
	errno = 0; \
	if (!(c)) \
		t_error("%s failed (errno = %d)\n", #c, errno); \
} while(0)

int main(void)
{
	char tmp[] = "fdopen.tmp";
	char foo[6];
	int fd;
	FILE *f;

	TEST((fd = open(tmp, O_RDWR | O_CREAT | O_EXCL, 0600)) > 2);
	TEST(write(fd, "hello", 6)==6);
	TEST(f = fdopen(fd, "rb"));
	if (f) {
		TEST(ftello(f)==6);
		TEST(fseeko(f, 0, SEEK_SET)==0);
		TEST(fgets(foo, sizeof foo, f));
		if (strcmp(foo,"hello") != 0)
			t_error("fgets read back: \"%s\"; wanted: \"hello\"\n", foo);
		fclose(f);
	}
	if (fd > 2)
		TEST(unlink(tmp) != -1);

	/* Beyond libc-test: a closed descriptor and a mode its access forbids. */
	errno = 0;
	if (fdopen(99, "r") != NULL || errno != EBADF)
		t_error("fdopen of a closed descriptor: errno %d\n", errno);
	TEST((fd = open(tmp, O_WRONLY | O_CREAT, 0600)) > 2);
	errno = 0;
	if (fdopen(fd, "r") != NULL || errno != EINVAL)
		t_error("fdopen for reading of a write-only descriptor: errno %d\n", errno);
	errno = 0;
	if (fdopen(fd, "q") != NULL || errno != EINVAL)
		t_error("fdopen with a bad mode: errno %d\n", errno);
	close(fd);
	unlink(tmp);
	return t_status;
}
