/*
 * qsort_r with glibc's argument order, elements larger than qsort's copy
 * buffer, and bsearch.
 */

#define _GNU_SOURCE
#include <stdlib.h>
#include <string.h>
#include "check.h"

/* The context counts calls and says which way to sort. */
struct context {
	int calls;
	int direction;
};

static int by_int(const void *a, const void *b, void *arg)
{
	struct context *ctx = arg;
	int x = *(const int *)a, y = *(const int *)b;
	ctx->calls++;
	return ctx->direction * ((x > y) - (x < y));
}

/* Larger than the 256 bytes a rotation copies at once. */
struct big {
	int key;
	unsigned char fill[1021];
};

static int by_key(const void *a, const void *b)
{
	const struct big *x = a, *y = b;
	return (x->key > y->key) - (x->key < y->key);
}

static int compare_int(const void *a, const void *b)
{
	int x = *(const int *)a, y = *(const int *)b;
	return (x > y) - (x < y);
}

static struct big bigs[300];

int main(void)
{
	int values[1000];
	for (int i = 0; i < 1000; i++)
		values[i] = (i * 7919) % 1000;

	struct context ctx = { 0, 1 };
	qsort_r(values, 1000, sizeof *values, by_int, &ctx);
	for (int i = 0; i < 1000; i++)
		CHECK(values[i] == i);
	/* n log2 n is about 10000; smoothsort stays within a small multiple. */
	CHECK(ctx.calls > 0 && ctx.calls < 40000);

	/* Already sorted input takes a few comparisons per element. */
	ctx.calls = 0;
	qsort_r(values, 1000, sizeof *values, by_int, &ctx);
	for (int i = 0; i < 1000; i++)
		CHECK(values[i] == i);
	CHECK(ctx.calls < 4000);

	ctx.direction = -1;
	qsort_r(values, 1000, sizeof *values, by_int, &ctx);
	for (int i = 0; i < 1000; i++)
		CHECK(values[i] == 999 - i);

	for (int i = 0; i < 300; i++) {
		bigs[i].key = (i * 101) % 300;
		memset(bigs[i].fill, bigs[i].key & 0xff, sizeof bigs[i].fill);
	}
	qsort(bigs, 300, sizeof *bigs, by_key);
	for (int i = 0; i < 300; i++) {
		CHECK(bigs[i].key == i);
		CHECK(bigs[i].fill[0] == (i & 0xff));
		CHECK(bigs[i].fill[1020] == (i & 0xff));
	}

	/* Zero elements, or zero width, touch nothing. */
	qsort(0, 0, sizeof(int), compare_int);
	qsort(values, 1000, 0, compare_int);

	int sorted[] = { 1, 3, 5, 7, 9, 11 };
	for (int key = 0; key <= 12; key++) {
		int *found = bsearch(&key, sorted, 6, sizeof *sorted, compare_int);
		if (key % 2)
			CHECK(found && *found == key);
		else
			CHECK(found == 0);
	}
	int key = 1;
	CHECK(bsearch(&key, sorted, 0, sizeof *sorted, compare_int) == 0);
	CHECK(bsearch(&key, sorted, 1, sizeof *sorted, compare_int) == sorted);
	return t_status;
}
