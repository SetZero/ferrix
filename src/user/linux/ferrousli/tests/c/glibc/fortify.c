/*
 * glibc's checked functions that Chrome's libraries are built to call, as
 * _FORTIFY_SOURCE rewrites them: the plain call with the object's size
 * after it. Each must do what the plain function does when the size holds,
 * and with an argument naming one, be handed an object too small and abort
 * with glibc's message, before anything is written.
 */

#define _GNU_SOURCE
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <unistd.h>

#include "check.h"

char *__stpcpy_chk(char *, const char *, size_t);
char *__strncpy_chk(char *, const char *, size_t, size_t);
char *__strncat_chk(char *, const char *, size_t, size_t);
void __explicit_bzero_chk(void *, size_t, size_t);
long __fdelt_chk(long);
char *__getcwd_chk(char *, size_t, size_t);
int __getgroups_chk(int, gid_t *, size_t);
int __poll_chk(struct pollfd *, nfds_t, int, size_t);
ssize_t __read_chk(int, void *, size_t, size_t);
ssize_t __readlinkat_chk(int, const char *, char *, size_t, size_t);
char *__realpath_chk(const char *, char *, size_t);
char *__fgets_chk(char *, size_t, int, FILE *);
char *__fgets_unlocked_chk(char *, size_t, int, FILE *);
int __openat_2(int, const char *, int);
int __openat64_2(int, const char *, int);

/* A file of known lines for the fgets checks. */
static FILE *lines(void)
{
	FILE *f = tmpfile();
	CHECK(f != NULL);
	fputs("abc\nabcdefgh\n", f);
	rewind(f);
	return f;
}

static int abort_case(const char *which)
{
	char small[4], line[5];
	long fd = 1024;
	struct pollfd p[1];
	FILE *f;

	if (strcmp(which, "stpcpy") == 0)
		__stpcpy_chk(small, "four", sizeof small);
	else if (strcmp(which, "strncpy") == 0)
		__strncpy_chk(small, "x", 5, sizeof small);
	else if (strcmp(which, "strncat") == 0) {
		strcpy(small, "ab");
		__strncat_chk(small, "cdef", 2, sizeof small);
	} else if (strcmp(which, "fdelt") == 0)
		__fdelt_chk(fd);
	else if (strcmp(which, "read") == 0)
		__read_chk(0, small, 5, sizeof small);
	else if (strcmp(which, "poll") == 0)
		__poll_chk(p, 2, 0, sizeof p);
	else if (strcmp(which, "realpath") == 0)
		__realpath_chk("/", small, sizeof small);
	else if (strcmp(which, "fgets") == 0) {
		f = lines();
		/* "abc\n" fits in 5; "abcdefgh\n" does not. */
		CHECK(__fgets_chk(line, 5, 100, f) == line);
		__fgets_chk(line, 5, 100, f);
	} else if (strcmp(which, "openat") == 0)
		__openat_2(AT_FDCWD, "created", O_CREAT | O_WRONLY);
	else
		return 2;
	return 1;
}

int main(int argc, char **argv)
{
	char b[64], big[PATH_MAX];
	struct pollfd p[2];
	gid_t groups[64];
	FILE *f;
	int fd;

	if (argc > 1)
		return abort_case(argv[1]);

	CHECK(__stpcpy_chk(b, "abc", sizeof b) == b + 3 && strcmp(b, "abc") == 0);
	CHECK(__stpcpy_chk(b, "abc", 4) == b + 3);
	memset(b, 'z', sizeof b);
	CHECK(__strncpy_chk(b, "ab", 4, 4) == b && memcmp(b, "ab\0\0z", 5) == 0);
	strcpy(b, "ab");
	CHECK(__strncat_chk(b, "cdef", 2, 5) == b && strcmp(b, "abcd") == 0);
	__explicit_bzero_chk(b, 4, sizeof b);
	CHECK(b[0] == 0 && b[3] == 0);

	/* FD_SET's word: 64 descriptors to a long here, 32 on ARMv7-A. */
	CHECK(__fdelt_chk(0) == 0);
	CHECK(__fdelt_chk(1023) == 1023 / (8 * (long)sizeof(long)));

	CHECK(__getcwd_chk(big, sizeof big, sizeof big) == big && big[0] == '/');
	CHECK(__getgroups_chk(0, groups, 0) >= 0);
	CHECK(__getgroups_chk(64, groups, sizeof groups) >= 0);

	fd = open("file", O_CREAT | O_RDWR, 0600);
	CHECK(fd >= 0 && write(fd, "hello", 5) == 5 && lseek(fd, 0, SEEK_SET) == 0);
	CHECK(__read_chk(fd, b, 5, sizeof b) == 5 && memcmp(b, "hello", 5) == 0);
	p[0].fd = fd;
	p[0].events = POLLIN;
	p[1].fd = -1;
	p[1].events = 0;
	CHECK(__poll_chk(p, 2, 0, sizeof p) == 1 && p[0].revents == POLLIN);
	close(fd);

	CHECK(symlink("file", "link") == 0);
	CHECK(__readlinkat_chk(AT_FDCWD, "link", b, sizeof b, sizeof b) == 4 && memcmp(b, "file", 4) == 0);
	CHECK(__realpath_chk("link", big, sizeof big) == big && strcmp(big + strlen(big) - 5, "/file") == 0);
	/* A null destination comes with an unknown size. */
	char *owned = __realpath_chk("/", NULL, (size_t)-1);
	CHECK(owned != NULL && strcmp(owned, "/") == 0);
	free(owned);

	fd = __openat_2(AT_FDCWD, "file", O_RDONLY);
	CHECK(fd >= 0);
	close(fd);
	fd = __openat64_2(AT_FDCWD, "file", O_RDONLY | O_CLOEXEC);
	CHECK(fd >= 0);
	close(fd);

	/* A line that fits the object, one cut by n, and one that ends exactly
	 * at the object's last byte. */
	f = lines();
	CHECK(__fgets_chk(b, sizeof b, sizeof b, f) == b && strcmp(b, "abc\n") == 0);
	CHECK(__fgets_unlocked_chk(b, 4, 4, f) == b && strcmp(b, "abc") == 0);
	CHECK(__fgets_chk(b, 7, 100, f) == b && strcmp(b, "defgh\n") == 0);
	CHECK(__fgets_chk(b, 7, 100, f) == NULL);
	fclose(f);

	return t_status;
}
