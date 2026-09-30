/*
 * memmem, with needles short and long, periodic and not, and bytes above 127.
 *
 * Adapted from libc-test's src/functional/string_memmem.c (MIT), with its
 * checks rewritten to use check.h.
 */

#define _GNU_SOURCE
#include <string.h>
#include "check.h"

/* Not found within the first strlen(s) bytes, though it is in s tail. */
#define N(s, tail, sub) { \
	const char *p = s tail; \
	CHECK(memmem(p, strlen(s), sub, strlen(sub)) == NULL); \
}

/* Found at offset n. */
#define T(s, sub, n) { \
	const char *p = s; \
	const char *q = memmem(p, strlen(p), sub, strlen(sub)); \
	CHECK(q != NULL && q - p == n); \
}

int main(void)
{
	N("", "a", "a")
	N("a", "a", "aa")
	N("a", "b", "b")
	N("aa", "b", "ab")
	N("aa", "a", "aaa")
	N("aba", "b", "bab")
	N("abba", "b", "bab")
	N("abba", "ba", "aba")
	N("abc abc", "d", "abcd")
	N("0-1-2-3-4-5-6-7-8-9", "", "-3-4-56-7-8-")
	N("0-1-2-3-4-5-6-7-8-9", "", "-3-4-5+6-7-8-")
	N("_ _ _\xff_ _ _", "\x7f_", "_\x7f_")
	N("_ _ _\x7f_ _ _", "\xff_", "_\xff_")

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

	/* A zero-length needle is found even in a zero-length haystack. */
	CHECK(memmem("x", 0, "", 0) != NULL);
	/* NULs are ordinary bytes. */
	static const char nuls[] = "ab\0cd\0ef";
	CHECK(memmem(nuls, 8, "\0ef", 3) == nuls + 5);
	CHECK(memmem(nuls, 7, "\0ef", 3) == NULL);

	return t_status;
}
