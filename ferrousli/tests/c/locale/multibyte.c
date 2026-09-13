/*
 * The multibyte conversions in the C locale and in UTF-8: strict UTF-8,
 * characters split across calls, the separate hidden states, error returns
 * and errno, and where each string function leaves its pointers.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <locale.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include "check.h"

#define ERR ((size_t)-1)
#define PARTIAL ((size_t)-2)

static void c_locale(void)
{
	wchar_t wc, wcs[8];
	char buf[8];
	mbstate_t st;

	memset(&st, 0, sizeof st);
	CHECK(MB_CUR_MAX == 1);
	/* Every byte is a character, and 0x80 to 0xff are U+DF80 to U+DFFF. */
	CHECK(mbrtowc(&wc, "\xc3\xa9", 2, &st) == 1 && wc == 0xdfc3);
	CHECK(mbrtowc(&wc, "", 1, &st) == 0 && wc == 0);
	CHECK(mbtowc(&wc, "\xff", 1) == 1 && wc == 0xdfff);
	CHECK(mblen("\xf0\x9f\x98\x80", 4) == 1);
	CHECK(mbstowcs(wcs, "\xc3\xa9!", 8) == 3);
	CHECK(wcs[0] == 0xdfc3 && wcs[1] == 0xdfa9 && wcs[2] == '!' && wcs[3] == 0);
	CHECK(mbstowcs(NULL, "\xc3\xa9!", 0) == 3);
	/* Only ASCII and those 128 characters convert back. */
	CHECK(wcrtomb(buf, 0xdf80, &st) == 1 && (unsigned char)buf[0] == 0x80);
	CHECK(wcrtomb(buf, 'x', &st) == 1 && buf[0] == 'x');
	errno = 0;
	CHECK(wcrtomb(buf, 0xe9, &st) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(wcrtomb(buf, 0x20ac, &st) == ERR);
	CHECK(wcrtomb(buf, 0xdf7f, &st) == ERR);
	CHECK(wctomb(buf, 0xdfff) == 1 && (unsigned char)buf[0] == 0xff);
	CHECK(wcstombs(buf, (wchar_t[]){ 0xdfc3, 0xdfa9, 0 }, 8) == 2);
	CHECK(!strcmp(buf, "\xc3\xa9"));
	CHECK(btowc(0x80) == 0xdf80);
	CHECK(btowc(0xff) == 0xdfff);
	CHECK(btowc('a') == 'a');
	CHECK(btowc(EOF) == WEOF);
	CHECK(wctob(0xdfff) == 0xff);
	CHECK(wctob(0xe9) == EOF);
	CHECK(wctob(WEOF) == EOF);
}

static void utf8(void)
{
	wchar_t wc, wcs[16];
	char buf[16];
	mbstate_t st;
	const char *s, *p;
	const wchar_t *ws, *wp;

	/* A four-byte character, a byte at a time. */
	memset(&st, 0, sizeof st);
	CHECK(mbrtowc(&wc, "\xf0", 1, &st) == PARTIAL);
	CHECK(!mbsinit(&st));
	CHECK(mbrtowc(&wc, "\x9f", 1, &st) == PARTIAL);
	CHECK(mbrtowc(&wc, "\x98", 1, &st) == PARTIAL);
	wc = 0;
	CHECK(mbrtowc(&wc, "\x80" "z", 2, &st) == 1);
	CHECK(wc == 0x1f600);
	CHECK(mbsinit(&st));

	/* Two bytes, then one. */
	CHECK(mbrtowc(&wc, "\xe2\x82", 2, &st) == PARTIAL);
	CHECK(mbrtowc(&wc, "\xac", 1, &st) == 1 && wc == 0x20ac);

	/* No bytes is incomplete, and leaves the state alone. */
	CHECK(mbrtowc(&wc, "a", 0, &st) == PARTIAL);
	CHECK(mbsinit(&st));

	wc = 1;
	CHECK(mbrtowc(&wc, "", 1, &st) == 0 && wc == 0);
	CHECK(mbrtowc(NULL, "\xc3\xa9", 2, &st) == 2);

	/* A null string resets the state, and fails mid-character. */
	CHECK(mbrtowc(NULL, NULL, 0, &st) == 0);
	CHECK(mbrtowc(&wc, "\xc3", 1, &st) == PARTIAL);
	errno = 0;
	CHECK(mbrtowc(NULL, NULL, 0, &st) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(mbsinit(&st));

	/* An invalid byte resets the state too. */
	CHECK(mbrtowc(&wc, "\xe2", 1, &st) == PARTIAL);
	errno = 0;
	CHECK(mbrtowc(&wc, "\x28", 1, &st) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(mbsinit(&st));
	CHECK(mbrtowc(&wc, "\xf5\x80\x80\x80", 4, &st) == ERR);
	CHECK(mbrtowc(&wc, "\xf4\x90\x80\x80", 4, &st) == ERR);
	CHECK(mbrtowc(&wc, "\xed\xbf\xbf", 3, &st) == ERR);
	CHECK(mbrtowc(&wc, "\xc2\x7f", 2, &st) == ERR);
	CHECK(mbrtowc(&wc, "\xc2\xc0", 2, &st) == ERR);
	CHECK(mbrtowc(&wc, "\xff", 1, &st) == ERR);
	CHECK(mbrtowc(&wc, "\xed\x9f\xbf", 3, &st) == 3 && wc == 0xd7ff);
	CHECK(mbrtowc(&wc, "\xee\x80\x80", 3, &st) == 3 && wc == 0xe000);

	/* With a null state, mbrtowc and mbrlen each keep their own. */
	CHECK(mbrtowc(&wc, "\xc3", 1, NULL) == PARTIAL);
	CHECK(mbrlen("\xe2\x82", 2, NULL) == PARTIAL);
	CHECK(mbrlen("a", 1, &st) == 1);
	CHECK(mbrtowc(&wc, "\xa9", 1, NULL) == 1 && wc == 0xe9);
	CHECK(mbrlen("\xac", 1, NULL) == 1);
	CHECK(mbsinit(NULL));

	/* mbtowc keeps no state: a character must be whole. */
	CHECK(mbtowc(&wc, "\xc3\xa9", 2) == 2 && wc == 0xe9);
	CHECK(mbtowc(&wc, "\xf0\x9f\x98\x80", 8) == 4 && wc == 0x1f600);
	errno = 0;
	CHECK(mbtowc(&wc, "\xe2\x82\xac", 2) == -1);
	CHECK(errno == EILSEQ);
	errno = 0;
	CHECK(mbtowc(&wc, "a", 0) == -1);
	CHECK(errno == EILSEQ);
	CHECK(mbtowc(&wc, "\xe2\x28\xac", 3) == -1);
	CHECK(mbtowc(&wc, "\x80", 1) == -1);
	CHECK(mbtowc(NULL, NULL, 0) == 0);
	CHECK(mbtowc(&wc, "", 1) == 0);
	CHECK(mbtowc(NULL, "q", 1) == 1);
	CHECK(mblen("\xf0\x9f\x98\x80", 4) == 4);
	CHECK(mblen("\xf0\x9f\x98\x80", 3) == -1);
	CHECK(mblen(NULL, 0) == 0);

	/* Encoding, at each length's edges. */
	memset(buf, 'x', sizeof buf);
	CHECK(wcrtomb(buf, 0x7f, &st) == 1 && buf[0] == 0x7f);
	CHECK(wcrtomb(buf, 0x80, &st) == 2 && !memcmp(buf, "\xc2\x80", 2));
	CHECK(wcrtomb(buf, 0x7ff, &st) == 2 && !memcmp(buf, "\xdf\xbf", 2));
	CHECK(wcrtomb(buf, 0x800, &st) == 3 && !memcmp(buf, "\xe0\xa0\x80", 3));
	CHECK(wcrtomb(buf, 0xd7ff, &st) == 3 && !memcmp(buf, "\xed\x9f\xbf", 3));
	CHECK(wcrtomb(buf, 0xfffd, &st) == 3 && !memcmp(buf, "\xef\xbf\xbd", 3));
	CHECK(wcrtomb(buf, 0x10000, &st) == 4 && !memcmp(buf, "\xf0\x90\x80\x80", 4));
	CHECK(wcrtomb(buf, 0x10ffff, NULL) == 4 && !memcmp(buf, "\xf4\x8f\xbf\xbf", 4));
	CHECK(wcrtomb(NULL, 0x10ffff, NULL) == 1);
	errno = 0;
	CHECK(wcrtomb(buf, 0xd800, &st) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(wcrtomb(buf, 0xdf80, &st) == ERR);
	CHECK(wcrtomb(buf, 0x110000, &st) == ERR);
	CHECK(wcrtomb(buf, -1, &st) == ERR);
	CHECK(wctomb(buf, 0x20ac) == 3 && !memcmp(buf, "\xe2\x82\xac", 3));
	CHECK(wctomb(NULL, 0x20ac) == 0);
	errno = 0;
	CHECK(wctomb(buf, 0xdc00) == -1);
	CHECK(errno == EILSEQ);
	CHECK(wctomb(buf, 0) == 1 && buf[0] == 0);

	/* Only ASCII is a single byte. */
	CHECK(btowc('a') == 'a');
	CHECK(btowc(0) == 0);
	CHECK(btowc(0x80) == WEOF);
	CHECK(btowc(0xff) == WEOF);
	CHECK(btowc(EOF) == WEOF);
	CHECK(wctob('z') == 'z');
	CHECK(wctob(0xe9) == EOF);
	CHECK(wctob(0xdf80) == EOF);
	CHECK(wctob(WEOF) == EOF);

	/* Strings. */
	CHECK(mbstowcs(wcs, "h\xc3\xa9llo \xf0\x9f\x98\x80", 16) == 7);
	CHECK(wcs[1] == 0xe9 && wcs[6] == 0x1f600 && wcs[7] == 0);
	CHECK(mbstowcs(NULL, "h\xc3\xa9llo", 0) == 5);
	errno = 0;
	CHECK(mbstowcs(wcs, "a\xff" "b", 16) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(mbstowcs(NULL, "a\xc3", 0) == ERR);

	memset(&st, 0, sizeof st);
	s = "ab\xe2\x28";
	p = s;
	CHECK(mbsrtowcs(wcs, &p, 16, &st) == ERR);
	CHECK(p == s + 2);
	s = "a\xe2\x82";
	p = s;
	CHECK(mbsrtowcs(wcs, &p, 16, &st) == ERR);
	CHECK(p == s + 1);
	s = "h\xc3\xa9llo";
	p = s;
	CHECK(mbsrtowcs(wcs, &p, 2, &st) == 2);
	CHECK(p == s + 3);
	CHECK(mbsrtowcs(wcs, &p, 16, &st) == 3);
	CHECK(p == NULL && wcs[3] == 0);

	/* A character mbrtowc began, mbsrtowcs finishes, counting or storing. */
	s = "\x82\xac!";
	CHECK(mbrtowc(&wc, "\xe2", 1, &st) == PARTIAL);
	p = s;
	CHECK(mbsrtowcs(NULL, &p, 0, &st) == 2);
	CHECK(p == s && !mbsinit(&st));
	CHECK(mbsrtowcs(wcs, &p, 16, &st) == 2);
	CHECK(wcs[0] == 0x20ac && wcs[1] == '!' && wcs[2] == 0);
	CHECK(p == NULL && mbsinit(&st));

	memset(buf, 'x', sizeof buf);
	CHECK(wcstombs(buf, L"hé€", 16) == 6);
	CHECK(!strcmp(buf, "h\xc3\xa9\xe2\x82\xac"));
	CHECK(wcstombs(NULL, L"hé€\U0001f600", 0) == 10);
	errno = 0;
	CHECK(wcstombs(buf, (wchar_t[]){ 'a', 0xd800, 0 }, 16) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(wcstombs(NULL, (wchar_t[]){ 'a', 0x110000, 0 }, 0) == ERR);

	/* wcsrtombs stops before a character that does not fit. */
	ws = L"a€";
	wp = ws;
	memset(buf, 'x', sizeof buf);
	CHECK(wcsrtombs(buf, &wp, 2, &st) == 1);
	CHECK(wp == ws + 1 && buf[1] == 'x');
	wp = ws;
	CHECK(wcsrtombs(buf, &wp, 4, &st) == 4);
	CHECK(wp == ws + 2 && buf[4] == 'x');
	wp = ws;
	CHECK(wcsrtombs(buf, &wp, 5, &st) == 4);
	CHECK(wp == NULL && buf[4] == 0);
	ws = (wchar_t[]){ 'a', 0xdc00, 0 };
	wp = ws;
	errno = 0;
	CHECK(wcsrtombs(buf, &wp, 16, &st) == ERR);
	CHECK(errno == EILSEQ && wp == ws + 1);

	/* mbsnrtowcs stops at its byte limit, backing out of a split character. */
	s = "\xe2\x82\xac\xe2\x82\xac";
	p = s;
	memset(&st, 0, sizeof st);
	CHECK(mbsnrtowcs(wcs, &p, 4, 16, &st) == 1);
	CHECK(p == s + 3 && mbsinit(&st) && wcs[0] == 0x20ac);
	CHECK(mbsnrtowcs(wcs, &p, 3, 16, &st) == 1);
	CHECK(p == s + 6);
	p = s;
	CHECK(mbsnrtowcs(wcs, &p, 100, 1, &st) == 1);
	CHECK(p == s + 3);
	p = s;
	CHECK(mbsnrtowcs(NULL, &p, 100, 0, &st) == 2);
	CHECK(p == s);
	p = s;
	CHECK(mbsnrtowcs(wcs, &p, 100, 16, NULL) == 2);
	CHECK(p == NULL && wcs[2] == 0);
	s = "a\xff";
	p = s;
	errno = 0;
	CHECK(mbsnrtowcs(wcs, &p, 2, 16, &st) == ERR);
	CHECK(errno == EILSEQ && p == s + 1);

	/* wcsnrtombs stops at its character limit. */
	ws = L"€b";
	wp = ws;
	memset(buf, 'x', sizeof buf);
	CHECK(wcsnrtombs(buf, &wp, 1, 16, &st) == 3);
	CHECK(wp == ws + 1 && buf[3] == 'x');
	wp = ws;
	CHECK(wcsnrtombs(NULL, &wp, 16, 0, &st) == 4);
	CHECK(wp == ws);
	wp = ws;
	CHECK(wcsnrtombs(buf, &wp, 16, 2, &st) == 0);
	CHECK(wp == ws);
	wp = ws;
	CHECK(wcsnrtombs(buf, &wp, 16, 16, &st) == 4);
	CHECK(wp == NULL && !strcmp(buf, "\xe2\x82\xac" "b"));
	ws = (wchar_t[]){ 'a', 0xd800, 0 };
	wp = ws;
	errno = 0;
	CHECK(wcsnrtombs(buf, &wp, 16, 16, &st) == ERR);
	CHECK(errno == EILSEQ && wp == ws + 1);
}

int main(void)
{
	c_locale();
	CHECK(setlocale(LC_CTYPE, "C.UTF-8") != NULL);
	utf8();
	return t_status;
}
