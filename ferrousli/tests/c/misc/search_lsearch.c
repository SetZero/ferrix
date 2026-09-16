/*
 * lsearch and lfind, adapted from libc-test's src/functional/search_lsearch.c
 * (MIT).
 */

#define _XOPEN_SOURCE 700
#include <search.h>
#include <string.h>
#include "check.h"

#define W 80
static char tab[100][W];
static size_t nel;

#define set(k) do { \
	char *r = lsearch(k, tab, &nel, W, (int (*)(const void *, const void *))strcmp); \
	CHECK(strcmp(r, k) == 0); \
} while (0)

#define get(k) lfind(k, tab, &nel, W, (int (*)(const void *, const void *))strcmp)

int main(void)
{
	size_t n;

	CHECK(get("a") == 0);
	set("");
	set("a");
	set("b");
	set("abc");
	set("cd");
	set("e");
	set("ef");
	set("g");
	set("h");
	set("iiiiiiiiii");
	CHECK(get("a") == tab[1]);
	CHECK(get("c") == 0);
	n = nel;
	set("g");
	CHECK(nel == n);
	n = nel;
	set("j");
	CHECK(nel == n + 1);
	CHECK(strcmp(tab[n], "j") == 0);
	return t_status;
}
