/*
 * memset, bzero and explicit_bzero at 64 alignments and every length below
 * 244, checking the bytes on either side are untouched, and memset with
 * values beyond a byte.
 *
 * Adapted from libc-test's src/functional/string_memset.c (MIT), with its
 * checks rewritten to use check.h.
 */

#define _GNU_SOURCE
#include <stdint.h>
#include <string.h>
#include <strings.h>
#include "check.h"

#define N 500
static char buf[N];
static char buf2[N];

static void *(*volatile pmemset)(void *, int, size_t);
static void (*volatile pbzero)(void *, size_t);
static void (*volatile pexplicit_bzero)(void *, size_t);

static char *aligned(void *p)
{
	return (char *)(((uintptr_t)p + 63) & -64);
}

/* Returns whether the fill is right, so a failure reports once. */
static int test_align(int which, int align, int len)
{
	char *s = aligned(buf + 64) + align;
	char *want = aligned(buf2 + 64) + align;
	int fill = which == 0 ? '#' : 0;
	int i;

	for (i = 0; i < N; i++)
		buf[i] = buf2[i] = ' ';
	for (i = 0; i < len; i++)
		want[i] = fill;
	if (which == 0) {
		if (pmemset(s, '#', len) != s)
			return 0;
	} else if (which == 1) {
		pbzero(s, len);
	} else {
		pexplicit_bzero(s, len);
	}
	for (i = -64; i < len + 64; i++)
		if (s[i] != want[i])
			return 0;
	return 1;
}

static void test_value(int c)
{
	int i;

	pmemset(buf, c, 10);
	for (i = 0; i < 10; i++)
		CHECK((unsigned char)buf[i] == (unsigned char)c);
}

int main(void)
{
	int which, i, j;

	pmemset = memset;
	pbzero = bzero;
	pexplicit_bzero = explicit_bzero;

	for (which = 0; which < 3; which++)
		for (i = 0; i < 64; i++)
			for (j = 0; j < N - 256; j++)
				if (!test_align(which, i, j)) {
					CHECK(test_align(which, i, j));
					return t_status;
				}

	test_value('c');
	test_value(0);
	test_value(-1);
	test_value(-5);
	test_value(0xab);
	test_value(0x1ab);
	return t_status;
}
