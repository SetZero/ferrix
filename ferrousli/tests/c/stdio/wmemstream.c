/*
 * open_wmemstream: a wide-oriented stream into a growing wide buffer, whose
 * size counts wide characters, with a write after a seek back reporting the
 * position as the size, as musl's does.
 */

#define _GNU_SOURCE
#include <locale.h>
#include <stdio.h>
#include <stdlib.h>
#include <wchar.h>

#include "check.h"

int main(void)
{
	CHECK(setlocale(LC_ALL, "C.UTF-8") != NULL);

	wchar_t *buf = NULL;
	size_t size = 99;
	FILE *f = open_wmemstream(&buf, &size);
	CHECK(f != NULL);
	CHECK(buf != NULL && size == 0 && buf[0] == 0);
	CHECK(fwide(f, 0) > 0);

	CHECK(fwprintf(f, L"%ls %d", L"été", 42) == 6);
	CHECK(fflush(f) == 0);
	CHECK(size == 6 && wcscmp(buf, L"été 42") == 0);

	for (int i = 0; i < 100; i++)
		CHECK(fputwc(L'ü', f) == L'ü');
	CHECK(fflush(f) == 0);
	CHECK(size == 106 && buf[105] == L'ü' && buf[106] == 0);

	CHECK(fseek(f, 0, SEEK_SET) == 0);
	CHECK(fputws(L"E", f) >= 0);
	CHECK(fflush(f) == 0);
	CHECK(size == 1 && buf[0] == L'E' && buf[1] == L't');

	CHECK(fclose(f) == 0);
	free(buf);
	return t_status;
}
