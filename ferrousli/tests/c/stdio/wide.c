/*
 * Wide-character streams in a UTF-8 locale: fwide's orientation, fputwc and
 * fputws writing UTF-8, fgetwc and fgetws reading it back, ungetwc pushing a
 * whole character back, and an invalid byte sequence reported as EILSEQ.
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

	FILE *f = tmpfile();
	CHECK(f != NULL);
	CHECK(fwide(f, 0) == 0);
	CHECK(fputwc(L'é', f) == L'é');
	CHECK(fwide(f, 0) > 0);
	CHECK(fwide(f, -1) > 0);
	CHECK(fputws(L"héllo\nx", f) >= 0);
	CHECK(fflush(f) == 0);

	/* The bytes are UTF-8. */
	rewind(f);
	char bytes[16] = { 0 };
	CHECK(fread(bytes, 1, sizeof bytes, f) == 10);
	CHECK(memcmp(bytes, "\xc3\xa9h\xc3\xa9llo\nx", 10) == 0);

	rewind(f);
	CHECK(fgetwc(f) == L'é');
	wchar_t line[16];
	CHECK(fgetws(line, 16, f) == line);
	CHECK(wcscmp(line, L"héllo\n") == 0);
	CHECK(ungetwc(L'ü', f) == L'ü');
	CHECK(getwc(f) == L'ü');
	CHECK(fgetwc_unlocked(f) == L'x');
	CHECK(fgetwc(f) == WEOF && feof(f) && !ferror(f));
	CHECK(fgetws(line, 16, f) == NULL);
	CHECK(ungetwc(WEOF, f) == WEOF);
	fclose(f);

	/* A byte that starts no character. */
	FILE *bad = tmpfile();
	CHECK(bad != NULL);
	CHECK(fwrite("a\xff", 1, 2, bad) == 2);
	rewind(bad);
	CHECK(fgetwc(bad) == L'a');
	errno = 0;
	CHECK(fgetwc(bad) == WEOF && ferror(bad) && errno == EILSEQ);
	fclose(bad);

	/* A sequence cut short by the end of the file. */
	FILE *cut = tmpfile();
	CHECK(cut != NULL);
	CHECK(fwrite("\xc3", 1, 1, cut) == 1);
	rewind(cut);
	errno = 0;
	CHECK(fgetwc(cut) == WEOF && ferror(cut) && errno == EILSEQ);
	fclose(cut);

	return t_status;
}
