/*
 * Directories: created, entered, named by getcwd, removed, and used as the
 * base of the *at calls.
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
	char cwd[4096], inner[4096], buf[64];
	struct stat st;
	size_t len;
	int dir, fd;

	/* getcwd, and ERANGE when the name and its NUL do not fit. */
	CHECK(getcwd(cwd, sizeof cwd) == cwd && cwd[0] == '/');
	len = strlen(cwd);
	errno = 0;
	CHECK(getcwd(inner, len) == 0 && errno == ERANGE);
	errno = 0;
	CHECK(getcwd(inner, 1) == 0 && errno == ERANGE);
	CHECK(getcwd(inner, len + 1) == inner && strcmp(inner, cwd) == 0);
	errno = 0;
	CHECK(getcwd(inner, 0) == 0 && errno == EINVAL);

	/* mkdir, and entering the directory. */
	CHECK(mkdir("d", 0755) == 0);
	CHECK(stat("d", &st) == 0 && S_ISDIR(st.st_mode));
	errno = 0;
	CHECK(mkdir("d", 0755) == -1 && errno == EEXIST);
	errno = 0;
	CHECK(mkdir("missing/d", 0755) == -1 && errno == ENOENT);
	CHECK(chdir("d") == 0);
	CHECK(getcwd(inner, sizeof inner) == inner);
	CHECK(strncmp(inner, cwd, len) == 0 && strcmp(inner + len, "/d") == 0);
	CHECK(mkdir("e", 0700) == 0);
	CHECK(chdir("..") == 0);
	CHECK(getcwd(inner, sizeof inner) == inner && strcmp(inner, cwd) == 0);
	errno = 0;
	CHECK(chdir("missing") == -1 && errno == ENOENT);

	/* A directory with something in it cannot be removed. */
	errno = 0;
	CHECK(rmdir("d") == -1 && errno == ENOTEMPTY);

	/* The *at calls, relative to a directory descriptor. */
	dir = open("d", O_RDONLY | O_DIRECTORY);
	CHECK(dir >= 0);
	CHECK(mkdirat(dir, "f", 0700) == 0);
	fd = openat(dir, "f/file", O_WRONLY | O_CREAT | O_EXCL, 0600);
	CHECK(fd >= 0 && close(fd) == 0);
	CHECK(fstatat(dir, "f/file", &st, 0) == 0 && S_ISREG(st.st_mode));
	CHECK(faccessat(dir, "f/file", W_OK, 0) == 0);
	errno = 0;
	CHECK(unlinkat(dir, "f", 0) == -1 && errno == EISDIR);
	errno = 0;
	CHECK(unlinkat(dir, "f", AT_REMOVEDIR) == -1 && errno == ENOTEMPTY);
	errno = 0;
	CHECK(unlinkat(dir, "f/file", AT_REMOVEDIR) == -1 && errno == ENOTDIR);
	CHECK(unlinkat(dir, "f/file", 0) == 0);
	CHECK(unlinkat(dir, "f", AT_REMOVEDIR) == 0);

	/* fchdir. */
	CHECK(fchdir(dir) == 0);
	CHECK(getcwd(inner, sizeof inner) == inner && strcmp(inner + len, "/d") == 0);
	CHECK(rmdir("e") == 0);
	CHECK(chdir(cwd) == 0);
	CHECK(close(dir) == 0);
	errno = 0;
	CHECK(fchdir(dir) == -1 && errno == EBADF);
	CHECK(rmdir("d") == 0);
	errno = 0;
	CHECK(rmdir("d") == -1 && errno == ENOENT);

	/* A file is not a directory. */
	fd = open("plain", O_WRONLY | O_CREAT, 0600);
	CHECK(fd >= 0 && close(fd) == 0);
	errno = 0;
	CHECK(rmdir("plain") == -1 && errno == ENOTDIR);
	errno = 0;
	CHECK(chdir("plain") == -1 && errno == ENOTDIR);
	errno = 0;
	CHECK(open("plain", O_RDONLY | O_DIRECTORY) == -1 && errno == ENOTDIR);
	errno = 0;
	CHECK(fchdir(open("plain", O_RDONLY)) == -1 && errno == ENOTDIR);

	/* renameat, linkat, symlinkat, readlinkat, mkfifoat and mknodat. */
	CHECK(mkdir("g", 0700) == 0);
	dir = open("g", O_RDONLY | O_DIRECTORY);
	CHECK(dir >= 0);
	CHECK(renameat(AT_FDCWD, "plain", dir, "moved") == 0);
	CHECK(fstatat(dir, "moved", &st, 0) == 0 && S_ISREG(st.st_mode));
	CHECK(linkat(dir, "moved", AT_FDCWD, "linked", 0) == 0);
	CHECK(stat("linked", &st) == 0 && st.st_nlink == 2);
	CHECK(symlinkat("moved", dir, "sym") == 0);
	CHECK(readlinkat(dir, "sym", buf, sizeof buf) == 5 && memcmp(buf, "moved", 5) == 0);
	CHECK(fstatat(dir, "sym", &st, AT_SYMLINK_NOFOLLOW) == 0 && S_ISLNK(st.st_mode));
	CHECK(fstatat(dir, "sym", &st, 0) == 0 && S_ISREG(st.st_mode));
	CHECK(linkat(dir, "sym", dir, "followed", AT_SYMLINK_FOLLOW) == 0);
	CHECK(fstatat(dir, "followed", &st, AT_SYMLINK_NOFOLLOW) == 0 && S_ISREG(st.st_mode));
	CHECK(mkfifoat(dir, "fifo", 0600) == 0);
	CHECK(fstatat(dir, "fifo", &st, 0) == 0 && S_ISFIFO(st.st_mode));
	CHECK(mknodat(dir, "node", S_IFREG | 0600, 0) == 0);
	errno = 0;
	CHECK(renameat(dir, "missing", dir, "other") == -1 && errno == ENOENT);
	errno = 0;
	CHECK(mkdirat(-1, "x", 0700) == -1 && errno == EBADF);
	/* An absolute path ignores the directory descriptor. */
	CHECK(fstatat(-1, cwd, &st, 0) == 0 && S_ISDIR(st.st_mode));
	CHECK(close(dir) == 0);

	return t_status;
}
