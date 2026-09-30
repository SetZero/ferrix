/*
 * Numbers and times in wide strings: the wcstol and wcstod families, wide
 * space before the subject, a wide character ending it, end pointers and
 * errno; wcsftime; and POSIX.1-2024's wcslcpy and wcslcat.
 *
 * The wcstol cases follow libc-test's functional/wcstol.c (MIT).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <float.h>
#include <inttypes.h>
#include <limits.h>
#include <stdlib.h>
#include <time.h>
#include <wchar.h>

#include "check.h"

int main(void)
{
	wchar_t *end;

	/* Integers. */
	CHECK(wcstol(L"  -123abc", &end, 10) == -123 && *end == L'a');
	CHECK(wcstoul(L"　 0x1f", &end, 0) == 31 && *end == 0);
	CHECK(wcstoll(L"0777", NULL, 0) == 0777);
	CHECK(wcstoull(L"-1", NULL, 10) == ULLONG_MAX);
	errno = 0;
	CHECK(wcstol(L"99999999999999999999", &end, 10) == LONG_MAX && errno == ERANGE);
	CHECK(*end == 0);
	errno = 0;
	CHECK(wcstoimax(L"-9223372036854775809", NULL, 10) == INTMAX_MIN && errno == ERANGE);
	CHECK(wcstoumax(L"42é", &end, 10) == 42 && *end == 0xe9);
	const wchar_t *none = L"  é";
	errno = 0;
	CHECK(wcstol(none, &end, 10) == 0 && end == none);
	CHECK(wcstol(L"0x", &end, 16) == 0 && *end == L'x');

	/* Floating point. */
	CHECK(wcstod(L"\t2.5e1q", &end) == 25.0 && *end == L'q');
	CHECK(wcstof(L"0x1p-2", NULL) == 0.25f);
	CHECK(wcstold(L"1e4000", &end) > (long double)DBL_MAX && *end == 0);
	CHECK(wcstod(L"inf", NULL) > DBL_MAX);
	const wchar_t *word = L"word";
	CHECK(wcstod(word, &end) == 0.0 && end == word);

	/* wcsftime. */
	struct tm tm = { .tm_year = 124, .tm_mon = 8, .tm_mday = 16, .tm_hour = 9,
			 .tm_min = 5, .tm_sec = 7, .tm_wday = 1, .tm_yday = 259 };
	wchar_t buf[64];
	CHECK(wcsftime(buf, 64, L"%Y-%m-%d %H:%M:%S é %%", &tm) == 23);
	CHECK(wcscmp(buf, L"2024-09-16 09:05:07 é %") == 0);
	CHECK(wcsftime(buf, 64, L"%a %e|%-m|%Ey", &tm) == 11);
	CHECK(wcscmp(buf, L"Mon 16|9|24") == 0);
	CHECK(wcsftime(buf, 5, L"%Y-%m", &tm) == 0);
	CHECK(wcsftime(buf, 64, L"", &tm) == 0 && buf[0] == 0);

	/* wcslcpy and wcslcat. */
	wchar_t dst[8];
	CHECK(wcslcpy(dst, L"abc", sizeof dst / sizeof *dst) == 3 && wcscmp(dst, L"abc") == 0);
	CHECK(wcslcat(dst, L"defgh", 8) == 8 && wcscmp(dst, L"abcdefg") == 0);
	CHECK(wcslcpy(dst, L"0123456789", 4) == 10 && wcscmp(dst, L"012") == 0);
	CHECK(wcslcat(dst, L"x", 3) == 4 && wcscmp(dst, L"012") == 0);
	CHECK(wcslcpy(dst, L"z", 0) == 1 && wcscmp(dst, L"012") == 0);

	return t_status;
}
