/*
 * The life of a file: created exclusively, written, read back by offset,
 * sized and resized, renamed, linked, symbolically linked and unlinked, and
 * the errors on the way.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	struct stat st;
	char buf[64];
	int fd;

	umask(022);

	/* Created once; a second exclusive create fails. */
	fd = open("file", O_RDWR | O_CREAT | O_EXCL, 0644);
	CHECK(fd >= 0);
	errno = 0;
	CHECK(open("file", O_RDWR | O_CREAT | O_EXCL, 0644) == -1 && errno == EEXIST);
	errno = 0;
	CHECK(open("missing", O_RDONLY) == -1 && errno == ENOENT);

	/* Written, then read back from wherever the offset is moved. */
	CHECK(write(fd, "hello, world", 12) == 12);
	CHECK(lseek(fd, 0, SEEK_CUR) == 12);
	CHECK(lseek(fd, 7, SEEK_SET) == 7);
	CHECK(read(fd, buf, sizeof buf) == 5 && memcmp(buf, "world", 5) == 0);
	CHECK(read(fd, buf, sizeof buf) == 0);
	CHECK(lseek(fd, -5, SEEK_END) == 7);
	errno = 0;
	CHECK(lseek(fd, -100, SEEK_SET) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(lseek(fd, 0, 12345) == -1 && errno == EINVAL);

	/* pread and pwrite leave the offset where it was. */
	CHECK(lseek(fd, 3, SEEK_SET) == 3);
	CHECK(pwrite(fd, "W", 1, 7) == 1);
	CHECK(pread(fd, buf, 5, 7) == 5 && memcmp(buf, "World", 5) == 0);
	CHECK(pread(fd, buf, sizeof buf, 100) == 0);
	CHECK(lseek(fd, 0, SEEK_CUR) == 3);
	errno = 0;
	CHECK(pread(fd, buf, 1, -1) == -1 && errno == EINVAL);

	/* The size fstat reports, and ftruncate and truncate change. */
	CHECK(fstat(fd, &st) == 0);
	CHECK(S_ISREG(st.st_mode) && (st.st_mode & 0777) == 0644);
	CHECK(st.st_size == 12 && st.st_nlink == 1);
	CHECK(ftruncate(fd, 5) == 0);
	CHECK(fstat(fd, &st) == 0 && st.st_size == 5);
	CHECK(pread(fd, buf, sizeof buf, 0) == 5 && memcmp(buf, "hello", 5) == 0);
	CHECK(ftruncate(fd, 100) == 0);
	CHECK(fstat(fd, &st) == 0 && st.st_size == 100);
	buf[0] = 1;
	CHECK(pread(fd, buf, 1, 50) == 1 && buf[0] == 0);
	CHECK(truncate("file", 12) == 0);
	CHECK(stat("file", &st) == 0 && st.st_size == 12);
	errno = 0;
	CHECK(ftruncate(fd, -1) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(truncate("missing", 0) == -1 && errno == ENOENT);
	CHECK(fsync(fd) == 0 && fdatasync(fd) == 0);

	/* Renamed. */
	CHECK(rename("file", "renamed") == 0);
	errno = 0;
	CHECK(stat("file", &st) == -1 && errno == ENOENT);
	CHECK(stat("renamed", &st) == 0 && st.st_size == 12);
	errno = 0;
	CHECK(rename("missing", "other") == -1 && errno == ENOENT);

	/* Hard linked. */
	CHECK(link("renamed", "hard") == 0);
	CHECK(stat("hard", &st) == 0 && st.st_nlink == 2);
	errno = 0;
	CHECK(link("renamed", "hard") == -1 && errno == EEXIST);
	errno = 0;
	CHECK(link("missing", "other") == -1 && errno == ENOENT);

	/* Symbolically linked, and the link read back. */
	CHECK(symlink("renamed", "soft") == 0);
	CHECK(lstat("soft", &st) == 0 && S_ISLNK(st.st_mode) && st.st_size == 7);
	CHECK(stat("soft", &st) == 0 && S_ISREG(st.st_mode));
	memset(buf, 'x', sizeof buf);
	CHECK(readlink("soft", buf, sizeof buf) == 7);
	CHECK(memcmp(buf, "renamed", 7) == 0 && buf[7] == 'x');
	CHECK(readlink("soft", buf, 3) == 3 && memcmp(buf, "ren", 3) == 0);
	CHECK(readlink("soft", buf, 0) == 0);
	errno = 0;
	CHECK(readlink("renamed", buf, sizeof buf) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(readlink("missing", buf, 0) == -1 && errno == ENOENT);
	errno = 0;
	CHECK(symlink("anything", "soft") == -1 && errno == EEXIST);
	/* A dangling link is a link all the same. */
	CHECK(symlink("nowhere", "dangling") == 0);
	CHECK(lstat("dangling", &st) == 0 && S_ISLNK(st.st_mode));
	errno = 0;
	CHECK(stat("dangling", &st) == -1 && errno == ENOENT);

	/* Access checks. */
	CHECK(access("renamed", F_OK) == 0);
	CHECK(access("renamed", R_OK | W_OK) == 0);
	CHECK(faccessat(AT_FDCWD, "soft", R_OK, AT_EACCESS) == 0);
	errno = 0;
	CHECK(access("missing", F_OK) == -1 && errno == ENOENT);
	errno = 0;
	CHECK(faccessat(AT_FDCWD, "renamed", F_OK, 0x40000) == -1 && errno == EINVAL);

	/* A file is not a directory. */
	errno = 0;
	CHECK(open("renamed/inside", O_RDONLY) == -1 && errno == ENOTDIR);
	errno = 0;
	CHECK(stat("renamed/inside", &st) == -1 && errno == ENOTDIR);
	errno = 0;
	CHECK(unlink("renamed/inside") == -1 && errno == ENOTDIR);

	/* Unlinked, while the descriptor stays usable. */
	CHECK(unlink("soft") == 0 && unlink("hard") == 0 && unlink("dangling") == 0);
	CHECK(fstat(fd, &st) == 0 && st.st_nlink == 1);
	CHECK(unlink("renamed") == 0);
	CHECK(fstat(fd, &st) == 0 && st.st_nlink == 0);
	CHECK(pread(fd, buf, 5, 0) == 5 && memcmp(buf, "hello", 5) == 0);
	errno = 0;
	CHECK(unlink("renamed") == -1 && errno == ENOENT);

	/* A closed descriptor. */
	CHECK(close(fd) == 0);
	errno = 0;
	CHECK(close(fd) == -1 && errno == EBADF);
	errno = 0;
	CHECK(write(fd, "x", 1) == -1 && errno == EBADF);
	errno = 0;
	CHECK(read(fd, buf, 1) == -1 && errno == EBADF);
	errno = 0;
	CHECK(fstat(fd, &st) == -1 && errno == EBADF);
	errno = 0;
	CHECK(lseek(fd, 0, SEEK_SET) == -1 && errno == EBADF);
	errno = 0;
	CHECK(ftruncate(fd, 0) == -1 && errno == EBADF);
	errno = 0;
	CHECK(fsync(-1) == -1 && errno == EBADF);

	/* creat opens for writing only, and truncates. */
	fd = creat("created", 0600);
	CHECK(fd >= 0);
	CHECK(write(fd, "abc", 3) == 3);
	errno = 0;
	CHECK(read(fd, buf, 1) == -1 && errno == EBADF);
	CHECK(close(fd) == 0);
	fd = creat("created", 0600);
	CHECK(fd >= 0 && fstat(fd, &st) == 0 && st.st_size == 0);
	CHECK(close(fd) == 0);

	return t_status;
}
