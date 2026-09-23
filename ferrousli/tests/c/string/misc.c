/*
 * The rest of string.h and strings.h: the copies that return their end,
 * bounded copies, splitting, version ordering, collation in the C locale,
 * case-blind comparison, swab and ffs.
 */

#define _GNU_SOURCE
#include <limits.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>
#include "check.h"

int main(void)
{
	char b[32];
	char *s, *rest;

	/* memccpy stops after the byte, and returns past it. */
	memset(b, '.', sizeof b);
	CHECK(memccpy(b, "ab:cd", ':', 5) == b + 3);
	CHECK(!memcmp(b, "ab:.", 4));
	CHECK(memccpy(b, "abcde", ':', 5) == NULL);
	CHECK(!memcmp(b, "abcde.", 6));
	CHECK(memccpy(b, "abcde", 'a' + 256, 5) == b + 1);

	/* mempcpy, stpcpy and stpncpy return the end. */
	CHECK(mempcpy(b, "xyz", 3) == b + 3);
	CHECK(stpcpy(b, "hello") == b + 5);
	CHECK(!strcmp(b, "hello"));
	memset(b, '.', sizeof b);
	CHECK(stpncpy(b, "ab", 5) == b + 2);
	CHECK(!memcmp(b, "ab\0\0\0.", 6));
	CHECK(stpncpy(b, "abcdef", 3) == b + 3);
	CHECK(!memcmp(b, "abc\0", 4));
	CHECK(strncpy(b, "zz", 0) == b);
	CHECK(b[0] == 'a');

	/* strcat, and strncat with a bound longer than the string. */
	strcpy(b, "ab");
	CHECK(strcat(b, "cd") == b);
	CHECK(strncat(b, "ef", 10) == b);
	CHECK(!strcmp(b, "abcdef"));
	CHECK(strncat(b, "gh", 0) == b);
	CHECK(!strcmp(b, "abcdef"));

	/* strnlen stops at n, and at the NUL. */
	CHECK(strnlen("abc", 0) == 0);
	CHECK(strnlen("abc", 2) == 2);
	CHECK(strnlen("abc", 100) == 3);

	/* memchr and memrchr treat c as a byte and NUL as ordinary. */
	static const char nul[] = "ab\0cd";
	CHECK(memchr(nul, 'c', 5) == nul + 3);
	strcpy(b, "abcabc");
	CHECK(memchr(b, 'c' - 256, 6) == b + 2);
	CHECK(memrchr(b, 'c', 6) == b + 5);
	CHECK(memrchr(b, 'c', 0) == NULL);
	CHECK(memchr(b, 0, 7) == b + 6);
	CHECK(strchrnul(b, 'z') == b + 6);
	CHECK(strchrnul(b, 'b') == b + 1);

	/* strtok_r skips runs; strsep keeps empty fields. */
	strcpy(b, ";;a;;b;");
	CHECK(!strcmp(strtok_r(b, ";", &rest), "a"));
	CHECK(!strcmp(strtok_r(NULL, ";", &rest), "b"));
	CHECK(strtok_r(NULL, ";", &rest) == NULL);
	CHECK(strtok_r(NULL, ";", &rest) == NULL);
	strcpy(b, "a,,b");
	rest = b;
	CHECK(!strcmp(strsep(&rest, ","), "a"));
	CHECK(!strcmp(strsep(&rest, ","), ""));
	CHECK(!strcmp(strsep(&rest, ","), "b"));
	CHECK(rest == NULL);
	CHECK(strsep(&rest, ",") == NULL);
	strcpy(b, "abc");
	rest = b;
	CHECK((s = strsep(&rest, "")) == b && rest == NULL);

	/* strverscmp orders fractions, then numbers by value. */
	CHECK(strverscmp("000", "00") < 0);
	CHECK(strverscmp("00", "01") < 0);
	CHECK(strverscmp("01", "010") < 0);
	CHECK(strverscmp("010", "09") < 0);
	CHECK(strverscmp("09", "0") < 0);
	CHECK(strverscmp("0", "1") < 0);
	CHECK(strverscmp("9", "10") < 0);
	CHECK(strverscmp("item9", "item10") < 0);
	CHECK(strverscmp("1.2.10", "1.2.9") > 0);
	CHECK(strverscmp("abc", "abc") == 0);

	/* In the C locale, collation is byte order and strxfrm copies. */
	CHECK(strcoll("a", "b") < 0);
	CHECK(strcoll("\xff", "a") > 0);
	CHECK(strcoll("abc", "abc") == 0);
	memset(b, '.', sizeof b);
	CHECK(strxfrm(b, "abc", 3) == 3);
	CHECK(b[0] == '.');
	CHECK(strxfrm(b, "abc", 4) == 3);
	CHECK(!strcmp(b, "abc"));
	CHECK(strxfrm(NULL, "abcd", 0) == 4);

	/* Case-blind comparison folds ASCII letters only. */
	CHECK(strcasecmp("HeLLo", "hello") == 0);
	CHECK(strcasecmp("a", "B") < 0);
	CHECK(strcasecmp("_", "A") < 0);
	CHECK(strcasecmp("{", "Z") > 0);
	CHECK(strcasecmp("\xc9", "\xe9") != 0);
	CHECK(strncasecmp("ABCx", "abcY", 3) == 0);
	CHECK(strncasecmp("ABCx", "abcY", 4) < 0);
	CHECK(strncasecmp("x", "y", 0) == 0);

	/* bcopy takes its arguments the other way round, and may overlap. */
	strcpy(b, "abcdef");
	bcopy(b, b + 1, 4);
	CHECK(!strcmp(b, "aabcdf"));
	bzero(b + 1, 2);
	CHECK(!memcmp(b, "a\0\0cdf", 7));
	explicit_bzero(b, 1);
	CHECK(b[0] == 0);

	/* swab swaps pairs, and leaves an odd last byte alone. */
	memset(b, '.', sizeof b);
	swab("abcde", b, 5);
	CHECK(!memcmp(b, "badc.", 5));
	swab("ab", b, -2);
	CHECK(!memcmp(b, "badc.", 5));

	CHECK(ffs(0) == 0);
	CHECK(ffs(1) == 1);
	CHECK(ffs(0x80) == 8);
	CHECK(ffs((int)0x80000000u) == 32);
	CHECK(ffsl(0) == 0);
	CHECK(ffsl(LONG_MIN) == (int)(sizeof(long) * 8));
	CHECK(ffsll(3LL << 40) == 41);

	return t_status;
}
