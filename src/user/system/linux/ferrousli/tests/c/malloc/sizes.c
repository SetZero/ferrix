/*
 * Allocations of many sizes, below, at and above the large threshold, are
 * aligned to 16, hold at least what was asked, and do not overlap: each is
 * filled whole, through its usable size, before any is checked.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <malloc.h>
#include <stdint.h>
#include <stdlib.h>
#include "check.h"

/* More than malloc gives: 2^46 bytes, or 2^31 on a 32-bit target, which is
 * over PTRDIFF_MAX there. */
#define TOO_MUCH ((size_t)1 << (sizeof(size_t) == 8 ? 46 : 31))

#define COUNT 400

static void *ptrs[COUNT];
static size_t lens[COUNT];

static size_t size_for(int i)
{
	/* Every size from 0 to 200, then around each class edge, then large. */
	if (i < 201)
		return (size_t)i;
	if (i < 300)
		return (size_t)(i - 200) * 97;
	if (i < 380)
		return 131056 - 40 + (size_t)(i - 300);
	return (size_t)(i - 379) * 300000;
}

static unsigned char byte_for(int i, size_t at)
{
	return (unsigned char)(i * 31 + at);
}

int main(void)
{
	for (int i = 0; i < COUNT; i++) {
		size_t n = size_for(i);
		ptrs[i] = malloc(n);
		CHECK(ptrs[i] != NULL);
		CHECK(((uintptr_t)ptrs[i] & 15) == 0);
		lens[i] = malloc_usable_size(ptrs[i]);
		CHECK(lens[i] >= n);
		/* Rounding stays within a quarter, or a page, of the request. */
		CHECK(lens[i] <= n + n / 4 + 4096 + 32);
		unsigned char *p = ptrs[i];
		for (size_t at = 0; at < lens[i]; at++)
			p[at] = byte_for(i, at);
	}
	for (int i = 0; i < COUNT; i++) {
		unsigned char *p = ptrs[i];
		size_t bad = 0;
		for (size_t at = 0; at < lens[i]; at++)
			bad += p[at] != byte_for(i, at);
		CHECK(bad == 0);
	}
	/* Free the odd ones, refill the space, and check the even ones held. */
	for (int i = 1; i < COUNT; i += 2)
		free(ptrs[i]);
	for (int i = 1; i < COUNT; i += 2) {
		ptrs[i] = malloc(size_for(i));
		CHECK(ptrs[i] != NULL);
		lens[i] = malloc_usable_size(ptrs[i]);
		unsigned char *p = ptrs[i];
		for (size_t at = 0; at < lens[i]; at++)
			p[at] = (unsigned char)~byte_for(i, at);
	}
	for (int i = 0; i < COUNT; i += 2) {
		unsigned char *p = ptrs[i];
		size_t bad = 0;
		for (size_t at = 0; at < lens[i]; at++)
			bad += p[at] != byte_for(i, at);
		CHECK(bad == 0);
	}
	for (int i = 0; i < COUNT; i++)
		free(ptrs[i]);

	/*
	 * Sizes too large to map fail with ENOMEM. The compiler rejects such
	 * a call it can see, so it goes through a pointer it cannot.
	 */
	static void *(*volatile malloc_p)(size_t) = malloc;
	errno = 0;
	CHECK(malloc_p(SIZE_MAX) == NULL);
	CHECK(errno == ENOMEM);
	errno = 0;
	CHECK(malloc_p(PTRDIFF_MAX) == NULL);
	CHECK(errno == ENOMEM);
	errno = 0;
	CHECK(malloc_p(TOO_MUCH) == NULL);
	CHECK(errno == ENOMEM);
	return t_status;
}
