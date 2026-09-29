/*
 * A small trace buffer for tests that compare a sequence of results with a
 * string, and a way to generate that string from another C library.
 *
 * Built with -DGENERATE against the host's C library, a test prints each
 * trace, which is how the expected strings were written. Built normally, a
 * trace that differs is written to standard error with what was wanted.
 */

#ifndef FERROUSLI_TEST_TRACE_H
#define FERROUSLI_TEST_TRACE_H

#include <string.h>
#include <unistd.h>
#include "check.h"
#ifdef GENERATE
#include <stdio.h>
#endif

static char t_trace[8192];
static size_t t_len;

__attribute__((unused))
static void t_reset(void)
{
	t_len = 0;
	t_trace[0] = 0;
}

__attribute__((unused))
static void t_put(const char *s)
{
	size_t n = strlen(s);
	if (t_len + n >= sizeof t_trace)
		n = sizeof t_trace - 1 - t_len;
	memcpy(t_trace + t_len, s, n);
	t_len += n;
	t_trace[t_len] = 0;
}

__attribute__((unused))
static void t_putc(int c)
{
	char b[2] = {c, 0};
	t_put(b);
}

__attribute__((unused))
static void t_putint(long v)
{
	char b[24];
	int i = sizeof b - 1;
	unsigned long u = v < 0 ? -(unsigned long)v : (unsigned long)v;
	b[i] = 0;
	do {
		b[--i] = '0' + u % 10;
		u /= 10;
	} while (u);
	if (v < 0)
		b[--i] = '-';
	t_put(b + i);
}

/* Compares the trace with `want`, or prints it when generating. */
__attribute__((unused))
static void t_compare(const char *label, const char *want)
{
#ifdef GENERATE
	(void)want;
	fflush(stderr);
	printf("\t/* %s */\n\t\"%s\",\n", label, t_trace);
	fflush(stdout);
#else
	if (strcmp(t_trace, want) != 0) {
		write(2, label, strlen(label));
		write(2, ":\n got: ", 8);
		write(2, t_trace, strlen(t_trace));
		write(2, "\nwant: ", 7);
		write(2, want, strlen(want));
		write(2, "\n", 1);
		t_status = 1;
	}
#endif
}

#endif
