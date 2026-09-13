/*
 * glibc's checked printf functions, as programs built with _FORTIFY_SOURCE
 * call them: the flag and object size before the format. With an argument,
 * the named function is given an object too small, and must abort.
 */

#define _GNU_SOURCE
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "test.h"

int __printf_chk(int, const char *, ...);
int __fprintf_chk(FILE *, int, const char *, ...);
int __sprintf_chk(char *, int, size_t, const char *, ...);
int __snprintf_chk(char *, size_t, int, size_t, const char *, ...);
int __dprintf_chk(int, int, const char *, ...);
int __asprintf_chk(char **, int, const char *, ...);
int __vsnprintf_chk(char *, size_t, int, size_t, const char *, va_list);

#define CHECK(c) do { \
	if (!(c)) \
		t_error("%s failed\n", #c); \
} while (0)

static int checked_vsnprintf(char *s, size_t n, size_t object, const char *fmt, ...)
{
	va_list ap;
	va_start(ap, fmt);
	int r = __vsnprintf_chk(s, n, 1, object, fmt, ap);
	va_end(ap);
	return r;
}

int main(int argc, char **argv)
{
	char b[16];
	char *s = NULL;

	if (argc > 1 && strcmp(argv[1], "sprintf") == 0) {
		__sprintf_chk(b, 1, 4, "%s", "too long");
		return 1;
	}
	if (argc > 1 && strcmp(argv[1], "snprintf") == 0) {
		__snprintf_chk(b, 8, 1, 4, "x");
		return 1;
	}

	CHECK(__sprintf_chk(b, 1, sizeof b, "%d-%s", 7, "ok") == 4 && strcmp(b, "7-ok") == 0);
	CHECK(__sprintf_chk(b, 1, (size_t)-1, "%s", "unknown") == 7);
	CHECK(__snprintf_chk(b, 4, 1, sizeof b, "%s", "truncated") == 9 && strcmp(b, "tru") == 0);
	CHECK(checked_vsnprintf(b, sizeof b, sizeof b, "%.2f", 1.5) == 4 && strcmp(b, "1.50") == 0);
	CHECK(__asprintf_chk(&s, 1, "%x", 255) == 2 && s && strcmp(s, "ff") == 0);
	free(s);
	CHECK(__dprintf_chk(-1, 1, "x") == -1);
	CHECK(__printf_chk(1, "%s %d\n", "chk", 1) == 6);
	CHECK(__fprintf_chk(stdout, 2, "fchk %d\n", 2) == 7);
	return t_status;
}
