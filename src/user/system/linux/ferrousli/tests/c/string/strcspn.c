/*
 * strcspn and strspn, with empty sets, one-byte sets and bytes above 127.
 *
 * The strcspn cases are adapted from libc-test's
 * src/functional/string_strcspn.c (MIT), with its checks rewritten to use
 * check.h.
 */

#include <string.h>
#include "check.h"

int main(void)
{
	int i;
	char a[128];
	char s[256];

	for (i = 0; i < 128; i++)
		a[i] = (i + 1) & 127;
	for (i = 0; i < 256; i++)
		*((unsigned char *)s + i) = i + 1;

	CHECK(strcspn("", "") == 0);
	CHECK(strcspn("a", "") == 1);
	CHECK(strcspn("", "a") == 0);
	CHECK(strcspn("abc", "cde") == 2);
	CHECK(strcspn("abc", "ccc") == 2);
	CHECK(strcspn("abc", a) == 0);
	CHECK(strcspn("\xff\x80 abc", a) == 2);
	CHECK(strcspn(s, "\xff") == 254);

	CHECK(strspn("", "") == 0);
	CHECK(strspn("abc", "") == 0);
	CHECK(strspn("aaab", "a") == 3);
	CHECK(strspn("abc", a) == 3);
	CHECK(strspn("\xff\x80 abc", "\x80\xff") == 2);
	CHECK(strspn(s, a) == 127);
	CHECK(strpbrk("abc", "") == NULL);
	CHECK(strpbrk(s, "\x80") == s + 127);

	return t_status;
}
