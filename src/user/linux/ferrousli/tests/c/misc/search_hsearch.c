/*
 * hsearch, hcreate and hdestroy, and the reentrant hsearch_r family, adapted
 * from libc-test's src/functional/search_hsearch.c (MIT).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <search.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

static ENTRY *e;

#define set(k, v) do { \
	e = hsearch((ENTRY){.key = k, .data = (void *)v}, ENTER); \
	CHECK(e && strcmp(e->key, k) == 0); \
} while (0)

#define get(k) hsearch((ENTRY){.key = k, .data = 0}, FIND)

#define getdata(e) ((intptr_t)(e)->data)

int main(void)
{
	errno = 0;
	CHECK(!hcreate(-1) && errno == ENOMEM);
	CHECK(hcreate(13));
	/* A second table is refused while the first exists. */
	CHECK(!hcreate(13));
	set("", 0);
	set("a", 1);
	set("b", 2);
	set("abc", 3);
	set("cd", 4);
	set("e", 5);
	set("ef", 6);
	set("g", 7);
	set("h", 8);
	set("iiiiiiiiii", 9);
	CHECK(get("a") != 0);
	CHECK(get("c") == 0);
	set("g", 10);
	CHECK(e && getdata(e) == 7);
	set("g", 10);
	CHECK(e && getdata(e) == 7);
	set("j", 10);
	CHECK(e && getdata(e) == 10);
	hdestroy();
	CHECK(get("a") == 0);

	/* The reentrant table, grown well past its first size. */
	struct hsearch_data h;
	memset(&h, 0, sizeof h);
	CHECK(hcreate_r(4, &h));
	static char keys[300][8];
	for (int i = 0; i < 300; i++) {
		keys[i][0] = 'k';
		keys[i][1] = '0' + i / 100;
		keys[i][2] = '0' + i / 10 % 10;
		keys[i][3] = '0' + i % 10;
		ENTRY *r = 0;
		CHECK(hsearch_r((ENTRY){.key = keys[i], .data = (void *)(intptr_t)i}, ENTER, &r, &h));
		CHECK(r && r->key == keys[i]);
	}
	for (int i = 0; i < 300; i++) {
		ENTRY *r = 0;
		CHECK(hsearch_r((ENTRY){.key = keys[i]}, FIND, &r, &h));
		CHECK(r && getdata(r) == i);
	}
	ENTRY *r = (ENTRY *)1;
	CHECK(!hsearch_r((ENTRY){.key = "missing"}, FIND, &r, &h));
	CHECK(r == 0);
	CHECK(errno == ESRCH);
	hdestroy_r(&h);
	return t_status;
}
