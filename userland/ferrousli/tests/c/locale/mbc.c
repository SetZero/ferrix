/*
 * Adapted from libc-test's src/functional/mbc.c (MIT, Copyright © 2005-2014
 * Rich Felker, et al.): UTF-8 decoding rejects overlong forms, surrogates
 * and bad lead bytes, accepts the noncharacters, and a character mbrtowc
 * begins is finished by mbsrtowcs.
 */

#include <langinfo.h>
#include <locale.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include "check.h"

/* libc-test's T and TCHAR: a fresh state, then the call and its result. */
#define T(f, x) (void)(memset(&st, 0, sizeof st), CHECK((i = (f)) == (x)))

int main(void)
{
	const char *cs;
	int i;
	mbstate_t st, st2;
	wchar_t wc, wcs[32];

	(void)(
	setlocale(LC_CTYPE, "en_US.UTF-8") ||
	setlocale(LC_CTYPE, "en_GB.UTF-8") ||
	setlocale(LC_CTYPE, "en.UTF-8") ||
	setlocale(LC_CTYPE, "POSIX.UTF-8") ||
	setlocale(LC_CTYPE, "C.UTF-8") ||
	setlocale(LC_CTYPE, "UTF-8") ||
	setlocale(LC_CTYPE, "") );

	T(mbsrtowcs(wcs, (cs="abcdef",&cs), 3, &st), 3);
	T(mbsrtowcs(wcs, (cs="abcdef",&cs), 8, &st), 6);
	T(mbsrtowcs(NULL, (cs="abcdef",&cs), 2, &st), 6);

	CHECK(!strcmp(nl_langinfo(CODESET), "UTF-8"));
	if (t_status)
		return t_status;

	T(mbrtowc(&wc, "\x80", 1, &st), -1);
	T(mbrtowc(&wc, "\xc0", 1, &st), -1);

	T(mbrtowc(&wc, "\xc0\x80", 2, &st), -1);
	T(mbrtowc(&wc, "\xc0\xaf", 2, &st), -1);
	T(mbrtowc(&wc, "\xe0\x80\xaf", 3, &st), -1);
	T(mbrtowc(&wc, "\xf0\x80\x80\xaf", 4, &st), -1);
	T(mbrtowc(&wc, "\xf8\x80\x80\x80\xaf", 5, &st), -1);
	T(mbrtowc(&wc, "\xfc\x80\x80\x80\x80\xaf", 6, &st), -1);
	T(mbrtowc(&wc, "\xe0\x82\x80", 3, &st), -1);
	T(mbrtowc(&wc, "\xe0\x9f\xbf", 3, &st), -1);
	T(mbrtowc(&wc, "\xf0\x80\xa0\x80", 4, &st), -1);
	T(mbrtowc(&wc, "\xf0\x8f\xbf\xbd", 4, &st), -1);

	T(mbrtowc(&wc, "\xed\xa0\x80", 3, &st), -1);
	T(mbrtowc(&wc, "\xef\xbf\xbe", 3, &st), 3);
	T(mbrtowc(&wc, "\xef\xbf\xbf", 3, &st), 3);
	T(mbrtowc(&wc, "\xf4\x8f\xbf\xbe", 4, &st), 4);
	T(mbrtowc(&wc, "\xf4\x8f\xbf\xbf", 4, &st), 4);

	T(mbrtowc(&wc, "\xc2\x80", 2, &st), 2);
	T((mbrtowc(&wc, "\xc2\x80", 2, &st),wc), 0x80);
	T(mbrtowc(&wc, "\xe0\xa0\x80", 3, &st), 3);
	T((mbrtowc(&wc, "\xe0\xa0\x80", 3, &st),wc), 0x800);
	T(mbrtowc(&wc, "\xf0\x90\x80\x80", 4, &st), 4);
	T((mbrtowc(&wc, "\xf0\x90\x80\x80", 4, &st),wc), 0x10000);

	memset(&st2, 0, sizeof st2);
	T(mbrtowc(&wc, "\xc2", 1, &st2), -2);
	T(mbrtowc(&wc, "\x80", 1, &st2), 1);
	CHECK(wc == 0x80);

	memset(&st2, 0, sizeof st2);
	T(mbrtowc(&wc, "\xc2", 1, &st2), -2);
	T(mbsrtowcs(wcs, (cs="\xa0""abc",&cs), 32, &st2), 4);
	CHECK(wcs[0] == 0xa0);
	CHECK(wcs[1] == 'a');
	CHECK(cs == NULL);
	return t_status;
}
