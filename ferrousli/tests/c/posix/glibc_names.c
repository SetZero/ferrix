/*
 * glibc's large-file names and its pre-2.33 stat functions, which programs
 * built against glibc call. musl's headers do not declare them, so they are
 * declared here as glibc declares them.
 */

#define _XOPEN_SOURCE 700
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include "check.h"

int open64(const char *, int, ...);
int openat64(int, const char *, int, ...);
int creat64(const char *, mode_t);
off_t lseek64(int, off_t, int);
ssize_t pread64(int, void *, size_t, off_t);
ssize_t pwrite64(int, const void *, size_t, off_t);
int ftruncate64(int, off_t);
int truncate64(const char *, off_t);
int stat64(const char *, struct stat *);
int fstat64(int, struct stat *);
int lstat64(const char *, struct stat *);
int fstatat64(int, const char *, struct stat *, int);
void *mmap64(void *, size_t, int, int, int, off_t);
int __xstat(int, const char *, struct stat *);
int __fxstat(int, int, struct stat *);
int __lxstat(int, const char *, struct stat *);
int __fxstatat(int, int, const char *, struct stat *, int);

int main(void)
{
	struct stat st;
	char buf[4];
	void *m;
	int fd, other;

	fd = open64("f", O_RDWR | O_CREAT | O_EXCL, 0600);
	CHECK(fd >= 0);
	CHECK(pwrite64(fd, "abc", 3, 0x100000000) == 3);
	CHECK(fstat64(fd, &st) == 0 && st.st_size == 0x100000003);
	CHECK(lseek64(fd, 0, SEEK_END) == 0x100000003);
	CHECK(pread64(fd, buf, 3, 0x100000000) == 3 && memcmp(buf, "abc", 3) == 0);
	CHECK(ftruncate64(fd, 8192) == 0);
	CHECK(truncate64("f", 4096 * 3) == 0);
	CHECK(stat64("f", &st) == 0 && st.st_size == 4096 * 3);

	CHECK(symlink("f", "l") == 0);
	CHECK(lstat64("l", &st) == 0 && S_ISLNK(st.st_mode));
	CHECK(fstatat64(AT_FDCWD, "l", &st, 0) == 0 && S_ISREG(st.st_mode));

	/* The version argument is ignored. */
	CHECK(__xstat(1, "l", &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 4096 * 3);
	CHECK(__fxstat(1, fd, &st) == 0 && st.st_size == 4096 * 3);
	CHECK(__lxstat(1, "l", &st) == 0 && S_ISLNK(st.st_mode));
	CHECK(__fxstatat(1, AT_FDCWD, "l", &st, AT_SYMLINK_NOFOLLOW) == 0 && S_ISLNK(st.st_mode));
	errno = 0;
	CHECK(__xstat(1, "missing", &st) == -1 && errno == ENOENT);

	/* openat64 without a mode, as a caller that creates nothing passes it. */
	other = creat64("g", 0600);
	CHECK(other >= 0 && close(other) == 0);
	other = openat64(AT_FDCWD, "g", O_RDONLY);
	CHECK(other >= 0 && close(other) == 0);

	m = mmap64(0, 4096, PROT_READ, MAP_SHARED, fd, 4096);
	CHECK(m != MAP_FAILED && munmap(m, 4096) == 0);
	CHECK(close(fd) == 0);

	return t_status;
}
