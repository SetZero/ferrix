/*
 * CHECK, for test programs that must not depend on stdio.
 *
 * A failed check writes "file:line: check failed: expression" to standard
 * error with write(), and sets t_status. A program returns t_status from
 * main. The file is named without its directory, so the message does not
 * depend on where the tree is checked out.
 */

#ifndef FERROUSLI_TEST_CHECK_H
#define FERROUSLI_TEST_CHECK_H

#include <string.h>
#include <unistd.h>

__attribute__((unused))
static int t_status;

__attribute__((unused))
static void t_fail(const char *file, const char *line, const char *what)
{
	const char *base = file;
	for (const char *p = file; *p; p++)
		if (*p == '/')
			base = p + 1;
	write(2, base, strlen(base));
	write(2, ":", 1);
	write(2, line, strlen(line));
	write(2, ": check failed: ", 16);
	write(2, what, strlen(what));
	write(2, "\n", 1);
	t_status = 1;
}

#define T_STRING2(x) #x
#define T_STRING1(x) T_STRING2(x)
#define CHECK(cond) \
	((cond) ? (void)0 : t_fail(__FILE__, T_STRING1(__LINE__), #cond))

#endif
