/*
 * Permissions, the creation mask, special files and timestamps.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include "check.h"

static int mode_of(const char *path)
{
	struct stat st;
	if (stat(path, &st) != 0)
		return -1;
	return st.st_mode & 07777;
}

int main(void)
{
	struct stat st;
	struct timespec times[2];
	int fd;

	/* umask returns the mask it replaces, and applies to what is created. */
	umask(022);
	CHECK(umask(077) == 022);
	fd = open("masked", O_WRONLY | O_CREAT, 0666);
	CHECK(fd >= 0);
	CHECK(mode_of("masked") == 0600);
	CHECK(umask(022) == 077);
	CHECK(umask(01777) == 022);
	CHECK(umask(022) == 0777);

	/* chmod, fchmod and fchmodat. */
	CHECK(chmod("masked", 0751) == 0 && mode_of("masked") == 0751);
	CHECK(fchmod(fd, 0640) == 0 && mode_of("masked") == 0640);
	CHECK(fchmodat(AT_FDCWD, "masked", 0604, 0) == 0);
	CHECK(mode_of("masked") == 0604);
	CHECK(fchmodat(AT_FDCWD, "masked", 0600, AT_SYMLINK_NOFOLLOW) == 0 ||
	      errno == EOPNOTSUPP);
	errno = 0;
	CHECK(chmod("missing", 0600) == -1 && errno == ENOENT);
	errno = 0;
	CHECK(fchmod(-1, 0600) == -1 && errno == EBADF);
	errno = 0;
	CHECK(fchmodat(AT_FDCWD, "masked", 0600, 0x4000) == -1 && errno == EINVAL);

	/* A FIFO, and a regular file from mknod. */
	CHECK(mkfifo("fifo", 0600) == 0);
	CHECK(stat("fifo", &st) == 0 && S_ISFIFO(st.st_mode));
	errno = 0;
	CHECK(mkfifo("fifo", 0600) == -1 && errno == EEXIST);
	CHECK(mknod("node", S_IFREG | 0600, 0) == 0);
	CHECK(stat("node", &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 0);

	/* Timestamps, to the nanosecond. */
	times[0].tv_sec = 1000;
	times[0].tv_nsec = 5;
	times[1].tv_sec = 2000;
	times[1].tv_nsec = 7;
	CHECK(utimensat(AT_FDCWD, "masked", times, 0) == 0);
	CHECK(stat("masked", &st) == 0);
	CHECK(st.st_atim.tv_sec == 1000 && st.st_atim.tv_nsec == 5);
	CHECK(st.st_mtim.tv_sec == 2000 && st.st_mtim.tv_nsec == 7);
	CHECK(st.st_atime == 1000 && st.st_mtime == 2000);

	/* UTIME_OMIT leaves one alone; futimens acts on the descriptor. */
	times[0].tv_nsec = UTIME_OMIT;
	times[1].tv_sec = 3000;
	times[1].tv_nsec = 0;
	CHECK(futimens(fd, times) == 0);
	CHECK(stat("masked", &st) == 0);
	CHECK(st.st_atim.tv_sec == 1000 && st.st_mtim.tv_sec == 3000);

	/* Null times mean now. */
	CHECK(futimens(fd, 0) == 0);
	CHECK(fstat(fd, &st) == 0 && st.st_mtim.tv_sec > 3000);
	errno = 0;
	CHECK(utimensat(AT_FDCWD, "missing", 0, 0) == -1 && errno == ENOENT);
	times[1].tv_nsec = 1000000000;
	errno = 0;
	CHECK(futimens(fd, times) == -1 && errno == EINVAL);

	CHECK(close(fd) == 0);
	return t_status;
}
