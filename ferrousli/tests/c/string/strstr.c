/*
 * strstr and strcasestr.
 *
 * The strstr cases are adapted from libc-test's
 * src/functional/string_strstr.c (MIT), with its checks rewritten to use
 * check.h.
 */

#define _GNU_SOURCE
#include <string.h>
#include "check.h"

/* Not found. */
#define N(s, sub) { \
	const char *p = s; \
	CHECK(strstr(p, sub) == NULL); \
}

/* Found at offset n. */
#define T(s, sub, n) { \
	const char *p = s; \
	const char *q = strstr(p, sub); \
	CHECK(q != NULL && q - p == n); \
}

static char big[2048];

int main(void)
{
	N("", "a")
	N("a", "aa")
	N("a", "b")
	N("aa", "ab")
	N("aa", "aaa")
	N("abba", "aba")
	N("abc abc", "abcd")
	N("0-1-2-3-4-5-6-7-8-9", "-3-4-56-7-8-")
	N("0-1-2-3-4-5-6-7-8-9", "-3-4-5+6-7-8-")
	N("_ _ _\xff_ _ _", "_\x7f_")
	N("_ _ _\x7f_ _ _", "_\xff_")

	T("", "", 0)
	T("abcd", "", 0)
	T("abcd", "a", 0)
	T("abcd", "b", 1)
	T("abcd", "c", 2)
	T("abcd", "d", 3)
	T("abcd", "ab", 0)
	T("abcd", "bc", 1)
	T("abcd", "cd", 2)
	T("ababa", "baba", 1)
	T("ababab", "babab", 1)
	T("abababa", "bababa", 1)
	T("abababab", "bababab", 1)
	T("ababababa", "babababa", 1)
	T("abbababab", "bababa", 2)
	T("abbababab", "ababab", 3)
	T("abacabcabcab", "abcabcab", 4)
	T("nanabanabanana", "aba", 3)
	T("nanabanabanana", "ban", 4)
	T("nanabanabanana", "anab", 1)
	T("nanabanabanana", "banana", 8)
	T("_ _\xff_ _", "_\xff_", 2)

	/* A long needle in a long haystack, which the search measures in
	 * stretches rather than all at once. */
	memset(big, 'a', sizeof big - 1);
	big[1500] = 'b';
	char needle[200];
	memset(needle, 'a', sizeof needle - 2);
	needle[sizeof needle - 2] = 'b';
	needle[sizeof needle - 1] = 0;
	CHECK(strstr(big, needle) == big + 1500 - 198);
	big[1500] = 'a';
	CHECK(strstr(big, needle) == NULL);

	const char *h = "The Quick Brown Fox";
	CHECK(strcasestr(h, "quick") == h + 4);
	CHECK(strcasestr(h, "FOX") == h + 16);
	CHECK(strcasestr(h, "") == h);
	CHECK(strcasestr(h, "foxes") == NULL);
	CHECK(strcasestr("", "a") == NULL);

	return t_status;
}
