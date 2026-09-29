/*
 * The wscanf family in a UTF-8 locale: widths and %n counting wide
 * characters; %ls, %lc and %l[ storing wide characters and %s, %c and %[
 * their UTF-8; %[ sets holding wide characters and ranges; literal wide
 * characters matching exactly; wide white space skipped; numbers; EOF on empty
 * input; and fwscanf reading a stream and giving back what ends a field.
 */

#define _GNU_SOURCE
#include <locale.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

#include "check.h"

int main(void)
{
	CHECK(setlocale(LC_ALL, "C.UTF-8") != NULL);
	wchar_t ws[16], wc = 0;
	char buf[16];
	int n = -1;
	double d = 0;

	CHECK(swscanf(L"42 été x", L"%d %ls %lc", &n, ws, &wc) == 3);
	CHECK(n == 42 && wcscmp(ws, L"été") == 0 && wc == L'x');

	CHECK(swscanf(L"été", L"%2ls", ws) == 1 && wcscmp(ws, L"ét") == 0);
	CHECK(swscanf(L"hé z", L"%s", buf) == 1 && strcmp(buf, "h\xc3\xa9") == 0);
	memset(buf, 0, sizeof buf);
	CHECK(swscanf(L"éx", L"%2c", buf) == 1 && memcmp(buf, "\xc3\xa9x", 3) == 0);

	CHECK(swscanf(L"éèabc", L"%l[èé]", ws) == 1);
	CHECK(wcscmp(ws, L"éè") == 0);
	CHECK(swscanf(L"abcxyz", L"%l[a-c]", ws) == 1 && wcscmp(ws, L"abc") == 0);
	CHECK(swscanf(L"xyz", L"%l[^a-c]", ws) == 1 && wcscmp(ws, L"xyz") == 0);

	n = -1;
	CHECK(swscanf(L"é5", L"é%d", &n) == 1 && n == 5);
	n = -1;
	CHECK(swscanf(L"è5", L"é%d", &n) == 0 && n == -1);

	n = -1;
	CHECK(swscanf(L"ééab", L"%*l[é]%n", &n) == 0 && n == 2);
	CHECK(swscanf(L"　 7", L"%d", &n) == 1 && n == 7);
	CHECK(swscanf(L"2.5e1", L"%lf", &d) == 1 && d == 25.0);
	CHECK(swscanf(L"", L"%d", &n) == EOF);

	FILE *f = tmpfile();
	CHECK(f != NULL);
	CHECK(fputs("\xc3\xa9t\xc3\xa9 12\xc3\xbc", f) >= 0);
	rewind(f);
	n = -1;
	CHECK(fwscanf(f, L"%ls %d", ws, &n) == 2);
	CHECK(wcscmp(ws, L"été") == 0 && n == 12);
	CHECK(fgetwc(f) == L'ü');
	CHECK(fwide(f, 0) > 0);
	fclose(f);

	return t_status;
}
