/*
 * wchar.h's string and memory functions: copies and their padding, signed
 * comparison, case-blind comparison beyond ASCII, searching, tokens,
 * overlapping moves and wcsdup.
 */

#define _GNU_SOURCE
#include <stdlib.h>
#include <wchar.h>
#include "check.h"

int main(void)
{
	wchar_t buf[16], tok[32];
	wchar_t *p, *save;

	CHECK(wcslen(L"") == 0);
	CHECK(wcslen(L"héllo") == 5);
	CHECK(wcsnlen(L"hello", 3) == 3);
	CHECK(wcsnlen(L"hi", 8) == 2);
	CHECK(wcsnlen((wchar_t[]){ 'a', 'b' }, 2) == 2);
	CHECK(wcsnlen(L"x", 0) == 0);

	CHECK(wcscpy(buf, L"abc") == buf && !wcscmp(buf, L"abc"));
	CHECK(wcpcpy(buf, L"xyz") == buf + 3 && buf[3] == 0);

	/* wcsncpy pads to n, and does not terminate a string that fills it. */
	wmemset(buf, 'Z', 16);
	CHECK(wcsncpy(buf, L"ab", 5) == buf);
	CHECK(buf[1] == 'b' && buf[2] == 0 && buf[4] == 0 && buf[5] == 'Z');
	wmemset(buf, 'Z', 16);
	CHECK(wcsncpy(buf, L"abcdef", 3) == buf && buf[2] == 'c' && buf[3] == 'Z');
	wmemset(buf, 'Z', 16);
	CHECK(wcpncpy(buf, L"ab", 5) == buf + 2 && buf[4] == 0 && buf[5] == 'Z');
	CHECK(wcpncpy(buf, L"abcdef", 3) == buf + 3);

	wcscpy(buf, L"ab");
	CHECK(wcscat(buf, L"cd") == buf && !wcscmp(buf, L"abcd"));
	CHECK(wcsncat(buf, L"efgh", 2) == buf && !wcscmp(buf, L"abcdef"));
	CHECK(wcsncat(buf, L"", 5) == buf && !wcscmp(buf, L"abcdef"));

	/* Characters compare as wchar_t, which is signed. */
	CHECK(wcscmp(L"abc", L"abc") == 0);
	CHECK(wcscmp(L"abc", L"abd") < 0);
	CHECK(wcscmp(L"abc", L"ab") > 0);
	CHECK(wcscmp((wchar_t[]){ -1, 0 }, L"a") < 0);
	CHECK(wcscmp((wchar_t[]){ 0x7fffffff, 0 }, (wchar_t[]){ -0x7fffffff - 1, 0 }) > 0);
	CHECK(wcsncmp(L"abcx", L"abcy", 3) == 0);
	CHECK(wcsncmp(L"abcx", L"abcy", 4) < 0);
	CHECK(wcsncmp(L"a", L"b", 0) == 0);
	CHECK(wmemcmp(L"ab\0c", L"ab\0d", 4) < 0);
	CHECK(wmemcmp((wchar_t[]){ -1 }, (wchar_t[]){ 1 }, 1) < 0);
	CHECK(wmemcmp(L"x", L"y", 0) == 0);

	CHECK(wcscasecmp(L"HÉLLO", L"héllo") == 0);
	CHECK(wcscasecmp(L"Σ", L"σ") == 0);
	CHECK(wcscasecmp(L"abc", L"ABD") < 0);
	CHECK(wcscasecmp(L"ab", L"abc") < 0);
	CHECK(wcsncasecmp(L"abcX", L"ABCy", 3) == 0);
	CHECK(wcsncasecmp(L"abcX", L"ABCy", 4) < 0);
	CHECK(wcsncasecmp(L"a", L"b", 0) == 0);

	/* Collation is code point order, and wcsxfrm a copy. */
	CHECK(wcscoll(L"a", L"b") < 0);
	CHECK(wcscoll(L"é", L"f") > 0);
	CHECK(wcsxfrm(buf, L"hello", 16) == 5 && !wcscmp(buf, L"hello"));
	wmemset(buf, 'Z', 16);
	CHECK(wcsxfrm(buf, L"hello", 3) == 5);
	CHECK(buf[0] == 'h' && buf[1] == 'e' && buf[2] == 0 && buf[3] == 'Z');
	CHECK(wcsxfrm(NULL, L"hello", 0) == 5);

	const wchar_t *s = L"hello, world";
	CHECK(wcschr(s, 'o') == s + 4);
	CHECK(wcschr(s, 'z') == NULL);
	CHECK(wcschr(s, 0) == s + 12);
	CHECK(wcsrchr(s, 'o') == s + 8);
	CHECK(wcsrchr(s, 'h') == s);
	CHECK(wcsrchr(s, 0) == s + 12);
	CHECK(wcsrchr(s, 'q') == NULL);
	CHECK(wcsspn(s, L"leh") == 4);
	CHECK(wcsspn(s, L"") == 0);
	CHECK(wcscspn(s, L", ") == 5);
	CHECK(wcscspn(s, L"") == 12);
	CHECK(wcspbrk(s, L"dw") == s + 7);
	CHECK(wcspbrk(s, L"xyz") == NULL);
	CHECK(wcsstr(s, L"world") == s + 7);
	CHECK(wcswcs(s, L"o, w") == s + 4);
	CHECK(wmemchr(s, 'w', 12) == s + 7);
	CHECK(wmemchr(s, 'w', 7) == NULL);
	CHECK(wmemchr(L"a\0b", 'b', 3) != NULL);

	wcscpy(tok, L"  one,two,, three ");
	CHECK((p = wcstok(tok, L" ,", &save)) == tok + 2 && !wcscmp(p, L"one"));
	CHECK((p = wcstok(NULL, L" ,", &save)) == tok + 6 && !wcscmp(p, L"two"));
	CHECK((p = wcstok(NULL, L" ,", &save)) == tok + 12 && !wcscmp(p, L"three"));
	CHECK(wcstok(NULL, L" ,", &save) == NULL);
	CHECK(save == NULL);
	CHECK(wcstok(NULL, L" ,", &save) == NULL);
	wcscpy(tok, L",,,");
	CHECK(wcstok(tok, L",", &save) == NULL);

	wchar_t m[8] = { 1, 2, 3, 4, 5, 6, 7, 8 };
	CHECK(wmemmove(m + 2, m, 5) == m + 2);
	CHECK(m[1] == 2 && m[2] == 1 && m[6] == 5 && m[7] == 8);
	CHECK(wmemmove(m, m + 2, 5) == m);
	CHECK(m[0] == 1 && m[4] == 5 && m[5] == 4);
	CHECK(wmemmove(m, m, 8) == m && m[3] == 4);
	CHECK(wmemcpy(buf, L"abcd", 5) == buf && !wcscmp(buf, L"abcd"));
	CHECK(wmemset(buf, 0x1f600, 3) == buf && buf[2] == 0x1f600 && buf[3] == 'd');

	wchar_t *d = wcsdup(L"dupé");
	CHECK(d != NULL && !wcscmp(d, L"dupé"));
	free(d);
	return t_status;
}
