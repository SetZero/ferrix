/*
 * malloc(0) returns unique pointers that can be freed, and free(NULL) does
 * nothing.
 *
 * Adapted from libc-test's src/regression/malloc-0.c (MIT), commit
 * 26031da0f83a2a3ed52190077931ee6c18dfd689: gnulib replaces a malloc whose
 * malloc(0) returns NULL, so programs expect it not to.
 */

#define _GNU_SOURCE
#include <malloc.h>
#include <stdlib.h>
#include "check.h"

int main(void)
{
	void *p = malloc(0);
	void *q = malloc(0);
	void *r = malloc(0);
	CHECK(p && q && r);
	CHECK(p != q && p != r && q != r);
	CHECK(((unsigned long)p & 15) == 0);
	CHECK(malloc_usable_size(p) < 64);
	free(q);
	free(p);
	free(r);

	free(NULL);
	CHECK(malloc_usable_size(NULL) == 0);

	/* Many at once stay distinct. */
	void *many[64];
	for (int i = 0; i < 64; i++) {
		many[i] = malloc(0);
		CHECK(many[i] != NULL);
		for (int j = 0; j < i; j++)
			CHECK(many[i] != many[j]);
	}
	for (int i = 0; i < 64; i++)
		free(many[i]);

	/* calloc with a zero count or size, too. */
	p = calloc(0, 10);
	q = calloc(10, 0);
	CHECK(p && q && p != q);
	free(p);
	free(q);
	return t_status;
}
