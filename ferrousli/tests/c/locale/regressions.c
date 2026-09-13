/*
 * libc-test's regression tests for this area (MIT, Copyright © 2005-2014
 * Rich Felker, et al.), from src/regression/, each under the name of its
 * file and the musl commit it guards.
 */

#define _GNU_SOURCE
#include <locale.h>
#include <string.h>
#include <wchar.h>
#include <wctype.h>
#include "check.h"

/* iswspace-null.c, musl d8e8f146: iswspace(0) is 0. */
static void iswspace_null(void)
{
	CHECK(iswspace(0) == 0);
}

/* mbsrtowcs-overflow.c, musl 211264e4: mbsrtowcs writes no more than asked. */
static void mbsrtowcs_overflow(void)
{
	wchar_t ws[] = L"XXXXX";
	const char *src = "abcd";
	const char *want = src + 4;

	CHECK(mbsrtowcs(ws, &src, 4, 0) == 4);
	CHECK(src == want);
	CHECK(wcscmp(ws, L"abcdX") == 0);
}

/* wcsncpy-read-overflow.c, musl e9813620: wcsncpy copies no more than n. */
static void wcsncpy_read_overflow(void)
{
	wchar_t dst[] = { 'a', 'a' };
	wchar_t src[] = { 0, 'b' };

	wcsncpy(dst, src, 1);
	CHECK(dst[1] == 'a');
}

/* wcsstr-false-negative.c, musl 476cd1d9: repetitive needles are found. */
static void wcsstr_false_negative(void)
{
	const wchar_t *haystack = L"playing play play play always";
	const wchar_t *needle = L"play play play";

	CHECK(wcsstr(haystack, needle) == haystack + 8);
}

/* uselocale-0.c, musl 63f4b9f1: uselocale(0) does not change the locale. */
static void uselocale_0(void)
{
	locale_t c = newlocale(LC_ALL_MASK, "C", 0);

	CHECK(c != 0);
	if (!c)
		return;
	CHECK(uselocale(c) != 0);
	locale_t l1 = uselocale(0);
	CHECK(l1 == c);
	locale_t l2 = uselocale(0);
	CHECK(l2 == l1);
}

int main(void)
{
	iswspace_null();
	mbsrtowcs_overflow();
	wcsncpy_read_overflow();
	wcsstr_false_negative();
	uselocale_0();
	return t_status;
}
