/*
 * CHECK, for test programs that must not depend on stdio.
 *
 * A failed check writes "file:line: check failed: expression" to standard
 * error with one write(), and sets t_status. A program returns t_status from
 * main. The file is named without its directory, so the message does not
 * depend on where the tree is checked out.
 *
 * Checks may fail on several threads at once: t_status is atomic, and each
 * message is written whole, so messages from two threads do not interleave.
 */

#ifndef FERROUSLI_TEST_CHECK_H
#define FERROUSLI_TEST_CHECK_H

#include <string.h>
#include <unistd.h>

__attribute__((unused))
static _Atomic int t_status;

__attribute__((unused))
static void t_fail(const char *file, const char *line, const char *what)
{
	char buf[512];
	unsigned long n = 0;
	const char *base = file;
	for (const char *p = file; *p; p++)
		if (*p == '/')
			base = p + 1;
	const char *parts[] = { base, ":", line, ": check failed: ", what, "\n" };
	for (unsigned i = 0; i < sizeof parts / sizeof *parts; i++)
		for (const char *p = parts[i]; *p && n < sizeof buf; p++)
			buf[n++] = *p;
	write(2, buf, n);
	t_status = 1;
}

#define T_STRING2(x) #x
#define T_STRING1(x) T_STRING2(x)
#define CHECK(cond) \
	((cond) ? (void)0 : t_fail(__FILE__, T_STRING1(__LINE__), #cond))

#endif
