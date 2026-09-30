/*
 * The small POSIX.1-2024 functions: a64l and l64a in radix 64, getsubopt,
 * secure_getenv in an ordinary program, strcasecmp_l and strncasecmp_l, and
 * tmpnam's names under /tmp.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <locale.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>

#include "check.h"

int main(void)
{
	/* Radix 64, least significant digit first. */
	CHECK(strcmp(l64a(0), "") == 0);
	CHECK(strcmp(l64a(1), "/") == 0);
	CHECK(strcmp(l64a(64), "./") == 0);
	CHECK(a64l("./") == 64);
	CHECK(a64l("/!") == 1);
	for (long v = -100000; v <= 100000; v += 777)
		CHECK(a64l(l64a(v)) == (long)(int)v);

	/* getsubopt over a mount-style option string. */
	char opts[] = "ro,size=64k,bogus,mode";
	char *const keys[] = { "ro", "size", "mode", NULL };
	char *p = opts, *value;
	CHECK(getsubopt(&p, keys, &value) == 0 && value == NULL);
	CHECK(getsubopt(&p, keys, &value) == 1 && value && strcmp(value, "64k") == 0);
	CHECK(getsubopt(&p, keys, &value) == -1);
	CHECK(getsubopt(&p, keys, &value) == 2 && value == NULL);
	CHECK(*p == 0);

	/* Not set-user-ID, so secure_getenv is getenv. */
	CHECK(setenv("FERROUSLI_SECURE", "yes", 1) == 0);
	CHECK(secure_getenv("FERROUSLI_SECURE") == getenv("FERROUSLI_SECURE"));
	CHECK(secure_getenv("FERROUSLI_SECURE") != NULL);

	/* Case-blind comparison in a locale. */
	locale_t c = newlocale(LC_ALL_MASK, "C", (locale_t)0);
	CHECK(c != (locale_t)0);
	CHECK(strcasecmp_l("Hello", "hELLO", c) == 0);
	CHECK(strcasecmp_l("abc", "ABD", c) < 0);
	CHECK(strncasecmp_l("abcX", "ABCy", 3, c) == 0);
	CHECK(strncasecmp_l("abcX", "ABCy", 4, c) != 0);
	freelocale(c);

	/* tmpnam names something that is not there, in either buffer. */
	struct stat st;
	char own[L_tmpnam];
	char *mine = tmpnam(own);
	CHECK(mine == own);
	CHECK(strncmp(own, "/tmp/tmpnam_", 12) == 0 && strlen(own) == 18);
	CHECK(lstat(own, &st) == -1 && errno == ENOENT);
	char *shared = tmpnam(NULL);
	CHECK(shared != NULL && shared != own);
	CHECK(strncmp(shared, "/tmp/tmpnam_", 12) == 0);

	return t_status;
}
