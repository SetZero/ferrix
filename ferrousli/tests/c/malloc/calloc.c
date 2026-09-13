/*
 * calloc returns zeroed memory, also when it reuses freed blocks, and calloc
 * and reallocarray fail with ENOMEM when the count times the size overflows.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

static const size_t sizes[] = {
	1, 15, 16, 17, 31, 64, 100, 128, 129, 1000, 4096, 50000, 131056,
	131057, 1 << 20,
};
#define SIZES (sizeof sizes / sizeof sizes[0])

static size_t nonzero(const unsigned char *p, size_t n)
{
	size_t bad = 0;
	for (size_t at = 0; at < n; at++)
		bad += p[at] != 0;
	return bad;
}

int main(void)
{
	void *ptrs[SIZES];

	for (int round = 0; round < 3; round++) {
		for (size_t i = 0; i < SIZES; i++) {
			ptrs[i] = malloc(sizes[i]);
			CHECK(ptrs[i] != NULL);
			memset(ptrs[i], 0xff, sizes[i]);
		}
		/* Freed in reverse, so reuse takes them in the same order. */
		for (size_t i = SIZES; i-- > 0;)
			free(ptrs[i]);
		for (size_t i = 0; i < SIZES; i++) {
			ptrs[i] = calloc(1, sizes[i]);
			CHECK(ptrs[i] != NULL);
			CHECK(nonzero(ptrs[i], sizes[i]) == 0);
			memset(ptrs[i], 0xff, sizes[i]);
		}
		for (size_t i = SIZES; i-- > 0;)
			free(ptrs[i]);
	}

	/* The count and size multiply. */
	int *ints = calloc(1000, sizeof(int));
	CHECK(ints != NULL);
	CHECK(nonzero((unsigned char *)ints, 1000 * sizeof(int)) == 0);
	free(ints);

	/*
	 * Overflow of the product. The compiler rejects such a call it can
	 * see, so these go through pointers it cannot.
	 */
	static void *(*volatile calloc_p)(size_t, size_t) = calloc;
	static void *(*volatile reallocarray_p)(void *, size_t, size_t) =
		reallocarray;
	errno = 0;
	CHECK(calloc_p(SIZE_MAX / 2 + 1, 2) == NULL);
	CHECK(errno == ENOMEM);
	errno = 0;
	CHECK(calloc_p(2, SIZE_MAX / 2 + 1) == NULL);
	CHECK(errno == ENOMEM);
	errno = 0;
	CHECK(calloc_p((size_t)1 << 32, (size_t)1 << 32) == NULL);
	CHECK(errno == ENOMEM);
	/* No overflow, but too much. */
	errno = 0;
	CHECK(calloc_p(SIZE_MAX, 1) == NULL);
	CHECK(errno == ENOMEM);

	char *p = malloc(10);
	memcpy(p, "ferrousli", 10);
	errno = 0;
	CHECK(reallocarray_p(p, SIZE_MAX / 4, 8) == NULL);
	CHECK(errno == ENOMEM);
	CHECK(memcmp(p, "ferrousli", 10) == 0);
	errno = 0;
	CHECK(reallocarray_p(NULL, (size_t)1 << 40, (size_t)1 << 40) == NULL);
	CHECK(errno == ENOMEM);
	free(p);
	return t_status;
}
