/*
 * strchr at eight alignments, with bytes above 127 and c beyond a byte.
 *
 * Adapted from libc-test's src/functional/string_strchr.c (MIT), with its
 * checks rewritten to use check.h.
 */

#include <stdint.h>
#include <string.h>
#include "check.h"

static char buf[512];

static void *aligned(void *p)
{
	return (void *)(((uintptr_t)p + 63) & -64);
}

static void *aligncpy(void *p, size_t len, size_t a)
{
	return memcpy((char *)aligned(buf) + a, p, len);
}

/* Not found, at any alignment. */
#define N(s, c) { \
	for (int align = 0; align < 8; align++) { \
		char *p = aligncpy(s, sizeof s, align); \
		CHECK(strchr(p, c) == NULL); \
	} \
}

/* Found at offset n, at every alignment. */
#define T(s, c, n) { \
	for (int align = 0; align < 8; align++) { \
		char *p = aligncpy(s, sizeof s, align); \
		char *q = strchr(p, c); \
		CHECK(q != NULL && q - p == n); \
	} \
}

int main(void)
{
	int i;
	char a[128];
	char s[256];

	for (i = 0; i < 128; i++)
		a[i] = (i + 1) & 127;
	for (i = 0; i < 256; i++)
		*((unsigned char *)s + i) = i + 1;

	N("\0aaa", 'a')
	N("a\0bb", 'b')
	N("ab\0c", 'c')
	N("abc\0d", 'd')
	N("abc abc\0x", 'x')
	N(a, 128)
	N(a, 255)

	T("", 0, 0)
	T("a", 'a', 0)
	T("a", 'a' + 256, 0)
	T("a", 0, 1)
	T("abb", 'b', 1)
	T("aabb", 'b', 2)
	T("aaabb", 'b', 3)
	T("aaaabb", 'b', 4)
	T("aaaaabb", 'b', 5)
	T("aaaaaabb", 'b', 6)
	T("abc abc", 'c', 2)
	T(s, 1, 0)
	T(s, 2, 1)
	T(s, 10, 9)
	T(s, 11, 10)
	T(s, 127, 126)
	T(s, 128, 127)
	T(s, 255, 254)
	T(s, 0, 255)

	return t_status;
}
