/*
 * stat on a directory, a device and a file.
 *
 * Adapted from libc-test's src/functional/stat.c (MIT). tmpfile() and stdio
 * are replaced by a file written with open and write.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	struct stat st;
	time_t t;
	int fd;

	CHECK(stat(".", &st) == 0);
	CHECK(S_ISDIR(st.st_mode));
	CHECK(st.st_nlink > 0);
	t = time(0);
	CHECK(st.st_ctime <= t);
	CHECK(st.st_mtime <= t);
	CHECK(st.st_atime <= t);

	CHECK(stat("/dev/null", &st) == 0);
	CHECK(S_ISCHR(st.st_mode));

	fd = open("f", O_RDWR | O_CREAT | O_EXCL, 0600);
	CHECK(fd >= 0);
	CHECK(write(fd, "hello", 5) == 5);
	CHECK(fstat(fd, &st) == 0);
	CHECK(st.st_uid == geteuid());
	CHECK(st.st_gid == getegid());
	CHECK(st.st_size == 5);
	CHECK(st.st_blksize > 0);
	CHECK(close(fd) == 0);

	return t_status;
}
