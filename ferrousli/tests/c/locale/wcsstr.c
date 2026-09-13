/*
 * Adapted from libc-test's src/functional/wcsstr.c (MIT, Copyright ©
 * 2005-2014 Rich Felker, et al.): needles that are absent, and needles found
 * at a known place, periodic ones included.
 */

#include <wchar.h>
#include "check.h"

/* The arguments are named in the check, so a failure says which case. */
#define N(s, sub) { \
	const wchar_t *p = s; \
	CHECK(((void)#s, (void)#sub, wcsstr(p, sub) == 0)); \
}

#define T(s, sub, n) { \
	const wchar_t *p = s; \
	CHECK(((void)#s, (void)#sub, wcsstr(p, sub) == p + n)); \
}

int main(void)
{
	N(L"", L"a")
	N(L"a", L"aa")
	N(L"a", L"b")
	N(L"aa", L"ab")
	N(L"aa", L"aaa")
	N(L"abba", L"aba")
	N(L"abc abc", L"abcd")
	N(L"0-1-2-3-4-5-6-7-8-9", L"-3-4-56-7-8-")
	N(L"0-1-2-3-4-5-6-7-8-9", L"-3-4-5+6-7-8-")
	N(L"_ _ _\xff_ _ _", L"_\x7f_")
	N(L"_ _ _\x7f_ _ _", L"_\xff_")

	T(L"", L"", 0)
	T(L"abcd", L"", 0)
	T(L"abcd", L"a", 0)
	T(L"abcd", L"b", 1)
	T(L"abcd", L"c", 2)
	T(L"abcd", L"d", 3)
	T(L"abcd", L"ab", 0)
	T(L"abcd", L"bc", 1)
	T(L"abcd", L"cd", 2)
	T(L"ababa", L"baba", 1)
	T(L"ababab", L"babab", 1)
	T(L"abababa", L"bababa", 1)
	T(L"abababab", L"bababab", 1)
	T(L"ababababa", L"babababa", 1)
	T(L"abbababab", L"bababa", 2)
	T(L"abbababab", L"ababab", 3)
	T(L"abacabcabcab", L"abcabcab", 4)
	T(L"nanabanabanana", L"aba", 3)
	T(L"nanabanabanana", L"ban", 4)
	T(L"nanabanabanana", L"anab", 1)
	T(L"nanabanabanana", L"banana", 8)
	T(L"_ _\xff_ _", L"_\xff_", 2)

	/* Beyond libc-test: long and negative characters, and a long needle. */
	T(L"xx\U0010ffff\U0010fffe\U0010ffffyy", L"\U0010fffe\U0010ffff", 3)
	N(L"xx\U0010ffff\U0010fffe", L"\U0010fffe\U0010ffff")
	T(L"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
	  L"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab", 14)
	{
		const wchar_t h[] = { 'a', -5, -6, 'b', 0 };
		const wchar_t n[] = { -5, -6, 0 };
		CHECK(wcsstr(h, n) == h + 1);
	}
	return t_status;
}
