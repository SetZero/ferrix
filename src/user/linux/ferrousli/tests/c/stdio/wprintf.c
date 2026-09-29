/*
 * The wprintf family in a UTF-8 locale: widths and precisions counting wide
 * characters for %s, %ls, %c and %lc; literal wide characters and %n counting
 * them; numbers from the narrow machinery; positional arguments; swprintf's -1
 * when the output does not fit; EILSEQ for a multibyte string that is not one;
 * and fwprintf writing UTF-8 to a stream it makes wide-oriented.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <locale.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

#include "check.h"

int main(void)
{
	CHECK(setlocale(LC_ALL, "C.UTF-8") != NULL);
	wchar_t buf[64];

	CHECK(swprintf(buf, 64, L"%d|%5.2ls|%-4lc|%s|%c|%x", 42, L"été", L'ü',
		       "h\xc3\xa9", 'A', 255) == 21);
	CHECK(wcscmp(buf, L"42|   ét|ü   |hé|A|ff") == 0);

	int n = -1;
	CHECK(swprintf(buf, 64, L"é%né", &n) == 2 && n == 1);
	CHECK(wcscmp(buf, L"éé") == 0);

	CHECK(swprintf(buf, 64, L"[%.1s][%.2s][%3s]", "h\xc3\xa9", "h\xc3\xa9", "\xc3\xa9") == 12);
	CHECK(wcscmp(buf, L"[h][hé][  é]") == 0);

	CHECK(swprintf(buf, 64, L"%2$ls-%1$d", 7, L"x") == 3 && wcscmp(buf, L"x-7") == 0);
	CHECK(swprintf(buf, 64, L"%5.1f|%-3d|%%", 2.25, 9) == 11);
	CHECK(wcscmp(buf, L"  2.2|9  |%") == 0);

	/* Output that does not fit fails, NUL-terminated. */
	CHECK(swprintf(buf, 3, L"%d", 1234) == -1 && buf[2] == 0);
	CHECK(swprintf(buf, 5, L"%d", 1234) == 4 && wcscmp(buf, L"1234") == 0);
	CHECK(swprintf(buf, 0, L"x") == -1);

	/* A multibyte %s that is no string. */
	errno = 0;
	CHECK(swprintf(buf, 64, L"%s", "\xff") == -1 && errno == EILSEQ);

	/* To a stream. */
	FILE *f = tmpfile();
	CHECK(f != NULL);
	CHECK(fwprintf(f, L"%ls=%d\n", L"é", 5) == 4);
	CHECK(fwide(f, 0) > 0);
	rewind(f);
	char bytes[8] = { 0 };
	CHECK(fread(bytes, 1, sizeof bytes, f) == 5);
	CHECK(memcmp(bytes, "\xc3\xa9=5\n", 5) == 0);
	fclose(f);

	return t_status;
}
