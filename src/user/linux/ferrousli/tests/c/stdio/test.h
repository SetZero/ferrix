/*
 * t_error and t_status for the stdio tests, adapted from libc-test's
 * src/common/test.h and src/common/print.c (MIT).
 *
 * A failed check prints "file:line: message" to standard output with
 * vsnprintf and write, and sets t_status, which main returns. Standard output
 * is expected to be empty, so any message fails the test and shows in its
 * report.
 */

#ifndef FERROUSLI_STDIO_TEST_H
#define FERROUSLI_STDIO_TEST_H

#include <stdarg.h>
#include <stdio.h>
#include <unistd.h>

__attribute__((unused))
static int t_status;

#define T_LOC2(l) __FILE__ ":" #l
#define T_LOC1(l) T_LOC2(l)
#define t_error(...) t_printf(T_LOC1(__LINE__) ": " __VA_ARGS__)

__attribute__((unused))
static int t_printf(const char *s, ...)
{
	va_list ap;
	char buf[512];
	int n;

	t_status = 1;
	va_start(ap, s);
	n = vsnprintf(buf, sizeof buf, s, ap);
	va_end(ap);
	if (n < 0)
		n = 0;
	else if (n >= (int)sizeof buf) {
		n = sizeof buf;
		buf[n - 1] = '\n';
		buf[n - 2] = '.';
		buf[n - 3] = '.';
		buf[n - 4] = '.';
	}
	return write(1, buf, n);
}

#endif
