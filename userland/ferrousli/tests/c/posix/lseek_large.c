/*
 * lseek, pwrite and pread with offsets past 2 GiB and 4 GiB.
 *
 * Adapted from libc-test's src/regression/lseek-large.c (MIT). tmpfile() is
 * replaced by a file in the working directory, and a sparse write and read at
 * a large offset are added.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	off_t a[] = { 0x7fffffff, 0x80000000, 0x80000001, 0xffffffff, 0x100000001, 0x1ffffffff, 0 };
	struct stat st;
	char c;
	int fd;

	fd = open("large", O_RDWR | O_CREAT | O_EXCL, 0600);
	CHECK(fd >= 0);
	for (int i = 0; a[i]; i++)
		CHECK(lseek(fd, a[i], SEEK_SET) == a[i]);

	CHECK(pwrite(fd, "x", 1, 0x100000000) == 1);
	CHECK(fstat(fd, &st) == 0 && st.st_size == 0x100000001);
	CHECK(lseek(fd, 0, SEEK_END) == 0x100000001);
	CHECK(pread(fd, &c, 1, 0x100000000) == 1 && c == 'x');
	CHECK(pread(fd, &c, 1, 0x80000000) == 1 && c == 0);
	CHECK(ftruncate(fd, 0) == 0);
	CHECK(close(fd) == 0);

	return t_status;
}
