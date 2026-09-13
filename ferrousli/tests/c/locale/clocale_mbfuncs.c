/*
 * Adapted from libc-test's src/functional/clocale_mbfuncs.c (MIT, Copyright
 * © 2005-2014 Rich Felker, et al.): in the C locale every byte converts to a
 * distinct wide character and back, no other character below U+110000
 * converts to a byte, and the high bytes' characters belong to no class.
 */

#include <limits.h>
#include <locale.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include <wctype.h>
#include "check.h"

int main(void)
{
	int i, j;
	mbstate_t st;
	wchar_t wc, map[257], wtmp[257];
	char s[MB_LEN_MAX*256];
	int ni_errors = 0;

	setlocale(LC_CTYPE, "C");
	CHECK(MB_CUR_MAX == 1);

	for (i=0; i<256; i++) {
		memset(&st, 0, sizeof st);
		CHECK(mbrtowc(&wc, &(char){i}, 1, &st) == (size_t)!!i);
		map[i] = btowc(i);
		CHECK((wint_t)map[i] != WEOF);
		for (j=0; j<i; j++)
			CHECK(map[j] != map[i]);
	}
	/* The original leaves this unset while wcschr reads through it. */
	map[256] = 0;

	for (i=0; i<256; i++)
		CHECK(wctob(map[i]) == i);

	for (i=0; i<0x110000; i++) {
		if (wcschr(map+1, i)) continue;
		if (wctob(i) != EOF)
			ni_errors++;
		memset(&st, 0, sizeof st);
		if (wcrtomb(s, i, &st) != (size_t)-1)
			ni_errors++;
	}
	CHECK(ni_errors == 0);

	memset(&st, 0, sizeof st);
	CHECK(wcsrtombs(s, &(const wchar_t *){map+1}, sizeof s, &st) == 255);
	CHECK(mbsrtowcs(wtmp, &(const char *){s}, 256, &st) == 255);
	CHECK(!memcmp(map+1, wtmp, 256*sizeof(*map)));

	for (i=128; i<256; i++) {
		CHECK(!iswalnum(map[i]));
		CHECK(!iswalpha(map[i]));
		CHECK(!iswblank(map[i]));
		CHECK(!iswcntrl(map[i]));
		CHECK(!iswdigit(map[i]));
		CHECK(!iswgraph(map[i]));
		CHECK(!iswlower(map[i]));
		CHECK(!iswprint(map[i]));
		CHECK(!iswpunct(map[i]));
		CHECK(!iswspace(map[i]));
		CHECK(!iswupper(map[i]));
		CHECK(!iswxdigit(map[i]));
	}
	return t_status;
}
