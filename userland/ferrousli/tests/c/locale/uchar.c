/*
 * uchar.h: mbrtoc16 and c16rtomb with surrogate pairs split across calls,
 * mbrtoc32 and c32rtomb, and their hidden states.
 */

#include <errno.h>
#include <locale.h>
#include <string.h>
#include <uchar.h>
#include <wchar.h>
#include "check.h"

#define ERR ((size_t)-1)
#define PARTIAL ((size_t)-2)
#define EARLIER ((size_t)-3)

int main(void)
{
	char16_t c16;
	char32_t c32;
	char buf[8];
	mbstate_t st;

	/* In C, a byte is a code unit. */
	memset(&st, 0, sizeof st);
	CHECK(mbrtoc32(&c32, "\x80", 1, &st) == 1 && c32 == 0xdf80);
	CHECK(mbrtoc16(&c16, "\xff", 1, &st) == 1 && c16 == 0xdfff);
	CHECK(c32rtomb(buf, 0xdfff, &st) == 1 && (unsigned char)buf[0] == 0xff);
	CHECK(c16rtomb(buf, 0xe9, &st) == ERR);

	CHECK(setlocale(LC_CTYPE, "C.UTF-8") != NULL);

	/* Above U+FFFF, the low surrogate is a second call consuming nothing. */
	memset(&st, 0, sizeof st);
	CHECK(mbrtoc16(&c16, "\xf0\x9f\x98\x80x", 5, &st) == 4 && c16 == 0xd83d);
	CHECK(!mbsinit(&st));
	CHECK(mbrtoc16(&c16, "x", 1, &st) == EARLIER && c16 == 0xde00);
	CHECK(mbsinit(&st));
	CHECK(mbrtoc16(&c16, "x", 1, &st) == 1 && c16 == 'x');
	CHECK(mbrtoc16(&c16, "\xe2\x82\xac", 3, &st) == 3 && c16 == 0x20ac);
	CHECK(mbrtoc16(&c16, "\xf0\x9f", 2, &st) == PARTIAL);
	CHECK(mbrtoc16(&c16, "\x98\x80", 2, &st) == 2 && c16 == 0xd83d);
	CHECK(mbrtoc16(NULL, "", 1, &st) == EARLIER);
	CHECK(mbrtoc16(&c16, "", 1, &st) == 0 && c16 == 0);
	errno = 0;
	CHECK(mbrtoc16(&c16, "\xed\xa0\x80", 3, &st) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(mbrtoc16(&c16, "\xf0\x90\x80\x80", 4, NULL) == 4 && c16 == 0xd800);
	CHECK(mbrtoc16(&c16, "", 1, NULL) == EARLIER && c16 == 0xdc00);
	CHECK(mbrtoc16(&c16, "\xf4\x8f\xbf\xbf", 4, NULL) == 4 && c16 == 0xdbff);
	CHECK(mbrtoc16(&c16, "", 1, NULL) == EARLIER && c16 == 0xdfff);

	/* And back. */
	memset(&st, 0, sizeof st);
	memset(buf, 0, sizeof buf);
	CHECK(c16rtomb(buf, 0xd83d, &st) == 0);
	CHECK(!mbsinit(&st));
	CHECK(c16rtomb(buf, 0xde00, &st) == 4 && !memcmp(buf, "\xf0\x9f\x98\x80", 4));
	CHECK(mbsinit(&st));
	CHECK(c16rtomb(buf, 0x20ac, &st) == 3 && !memcmp(buf, "\xe2\x82\xac", 3));
	errno = 0;
	CHECK(c16rtomb(buf, 0xde00, &st) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(c16rtomb(buf, 0xdbff, &st) == 0);
	errno = 0;
	CHECK(c16rtomb(buf, 'a', &st) == ERR);
	CHECK(errno == EILSEQ && mbsinit(&st));
	CHECK(c16rtomb(buf, 'a', &st) == 1 && buf[0] == 'a');
	CHECK(c16rtomb(NULL, 0, &st) == 1);
	CHECK(c16rtomb(buf, 0xd800, &st) == 0);
	CHECK(c16rtomb(NULL, 0, &st) == ERR);
	CHECK(mbsinit(&st));
	CHECK(c16rtomb(buf, 0xdbff, NULL) == 0);
	CHECK(c16rtomb(buf, 0xdfff, NULL) == 4 && !memcmp(buf, "\xf4\x8f\xbf\xbf", 4));

	/* A char32_t is a wide character. */
	CHECK(mbrtoc32(&c32, "\xf0\x9f\x98\x80", 4, &st) == 4 && c32 == 0x1f600);
	CHECK(mbrtoc32(&c32, "\xf0\x9f", 2, &st) == PARTIAL);
	CHECK(mbrtoc32(&c32, "\x98\x80", 2, &st) == 2 && c32 == 0x1f600);
	CHECK(mbrtoc32(NULL, "\xc3\xa9", 2, &st) == 2);
	CHECK(mbrtoc32(&c32, "", 1, &st) == 0 && c32 == 0);
	CHECK(mbrtoc32(&c32, "\xc3", 1, NULL) == PARTIAL);
	errno = 0;
	CHECK(mbrtoc32(NULL, NULL, 0, NULL) == ERR);
	CHECK(errno == EILSEQ);
	CHECK(mbrtoc32(NULL, NULL, 0, NULL) == 0);
	CHECK(c32rtomb(buf, 0x1f600, &st) == 4 && !memcmp(buf, "\xf0\x9f\x98\x80", 4));
	CHECK(c32rtomb(buf, 0xd800, &st) == ERR);
	CHECK(c32rtomb(buf, 0x110000, &st) == ERR);
	CHECK(c32rtomb(NULL, 0x1f600, &st) == 1);
	return t_status;
}
