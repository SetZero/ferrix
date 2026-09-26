/*
 * glibc's internal and older names that programs built against its headers
 * call in place of the standard ones: the pre-2.33 stat entry points, the
 * locale and C23 integer parsers, __strtoul_internal and __mbrlen; and
 * lchmod, which changes a file and refuses a symbolic link.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <locale.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <wchar.h>

#include "check.h"

long long __isoc23_strtoll_l(const char *, char **, int, locale_t);
unsigned long long __isoc23_strtoull_l(const char *, char **, int, locale_t);
long long strtoll_l(const char *, char **, int, locale_t);
unsigned long __strtoul_internal(const char *, char **, int, int);
size_t __mbrlen(const char *, size_t, mbstate_t *);
int lchmod(const char *, mode_t);
#if !defined(__arm__)
int __xstat64(int, const char *, struct stat *);
int __fxstat64(int, int, struct stat *);
int __lxstat64(int, const char *, struct stat *);
int __fxstatat64(int, int, const char *, struct stat *, int);
#endif

int main(void)
{
	locale_t c = newlocale(LC_ALL_MASK, "C", (locale_t)0);
	struct stat st;
	char *end;
	int fd;

	CHECK(c != (locale_t)0);
	CHECK(__isoc23_strtoll_l("-0b1010 ", &end, 0, c) == -10 && *end == ' ');
	CHECK(__isoc23_strtoull_l("0x10", &end, 0, c) == 16 && *end == 0);
#ifndef __GLIBC__
	/* glibc's own headers send strtoll_l to __isoc23_strtoll_l under
	 * _GNU_SOURCE; the plain name keeps C17's grammar. */
	CHECK(strtoll_l("0b1", &end, 0, c) == 0 && *end == 'b');
#endif
	errno = 0;
	CHECK(__isoc23_strtoll_l("99999999999999999999", &end, 10, c) == LLONG_MAX && errno == ERANGE);
	freelocale(c);
	CHECK(__strtoul_internal(" 4096k", &end, 0, 0) == 4096 && *end == 'k');

	CHECK(__mbrlen("a", 1, NULL) == 1);
	CHECK(__mbrlen("", 1, NULL) == 0);

	fd = open("file", O_CREAT | O_WRONLY, 0600);
	CHECK(fd >= 0 && write(fd, "abc", 3) == 3);
	CHECK(symlink("file", "link") == 0);
#if !defined(__arm__)
	CHECK(__xstat64(1, "link", &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 3);
	CHECK(__lxstat64(1, "link", &st) == 0 && S_ISLNK(st.st_mode));
	CHECK(__fxstat64(1, fd, &st) == 0 && st.st_size == 3);
	CHECK(__fxstatat64(1, AT_FDCWD, "link", &st, AT_SYMLINK_NOFOLLOW) == 0 && S_ISLNK(st.st_mode));
#endif
	close(fd);

	/* glibc since 2.32: the file changes, a symbolic link is refused. */
	CHECK(lchmod("file", 0640) == 0);
	CHECK(stat("file", &st) == 0 && (st.st_mode & 07777) == 0640);
	errno = 0;
	CHECK(lchmod("link", 0600) == -1 && errno == EOPNOTSUPP);
	return t_status;
}
