/*
 * The functions that look at a word at a time, at every alignment from 0 to
 * 15 and every length below 100, with the match or the NUL at every position
 * and non-NUL bytes after it, so a word read past the end cannot find a
 * false match there.
 */

#define _GNU_SOURCE
#include <stdint.h>
#include <string.h>
#include "check.h"

static char buf[512];

static char *aligned(void *p)
{
	return (char *)(((uintptr_t)p + 63) & -64);
}

/* Fills the buffer around a string of len bytes 'a'..'z' at align. */
static char *place(int align, int len)
{
	char *s = aligned(buf) + align;
	for (int i = 0; i < 256; i++)
		aligned(buf)[i] = '\x80';
	for (int i = 0; i < len; i++)
		s[i] = 'a' + i % 26;
	s[len] = 0;
	return s;
}

/* Returns whether every function is right for this string. */
static int check_one(int align, int len)
{
	char *s = place(align, len);
	int ok = 1;

	ok &= strlen(s) == (size_t)len;
	ok &= strnlen(s, len + 10) == (size_t)len;
	ok &= strnlen(s, len / 2) == (size_t)len / 2;
	ok &= strchr(s, 0) == s + len;
	ok &= strchr(s, 0x80) == NULL;
	ok &= strchrnul(s, 0x80) == s + len;
	ok &= strrchr(s, 0x80) == NULL;
	ok &= memchr(s, 0, len + 1) == s + len;
	ok &= memchr(s, 0, len) == NULL;
	ok &= memchr(s, 0x80, len + 1) == NULL;
	ok &= memrchr(s, 0x80, len + 1) == NULL;

	for (int at = 0; at < len; at++) {
		char saved = s[at];
		s[at] = '!';
		ok &= strchr(s, '!') == s + at;
		ok &= strchr(s, '!' + 256) == s + at;
		ok &= strchrnul(s, '!') == s + at;
		ok &= strrchr(s, '!') == s + at;
		ok &= memchr(s, '!', len) == s + at;
		ok &= memchr(s, '!', at) == NULL;
		ok &= memrchr(s, '!', len) == s + at;
		ok &= index(s, '!') == s + at;
		ok &= rindex(s, '!') == s + at;
		s[at] = saved;
	}
	return ok;
}

int main(void)
{
	for (int align = 0; align < 16; align++)
		for (int len = 0; len < 100; len++)
			if (!check_one(align, len)) {
				CHECK(check_one(align, len));
				return t_status;
			}
	return t_status;
}
