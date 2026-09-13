/*
 * realloc grows and shrinks across the size classes and the large threshold,
 * keeping the contents up to the smaller size.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <malloc.h>
#include <stdint.h>
#include <stdlib.h>
#include "check.h"

static unsigned char pattern(size_t at)
{
	return (unsigned char)(at * 7 + (at >> 8) * 13 + 1);
}

static void fill(unsigned char *p, size_t from, size_t to)
{
	for (size_t at = from; at < to; at++)
		p[at] = pattern(at);
}

static size_t damage(const unsigned char *p, size_t n)
{
	size_t bad = 0;
	for (size_t at = 0; at < n; at++)
		bad += p[at] != pattern(at);
	return bad;
}

static const size_t steps[] = {
	1, 16, 17, 100, 1000, 4000, 65536, 131056, 131057, 200000,
	1 << 20, 5 << 20, 300000, 131057, 131056, 5000, 64, 2, 0, 7,
	3 << 20, 24, 131056, 1 << 21, 131057,
};

int main(void)
{
	/* realloc(NULL, n) is malloc(n). */
	unsigned char *p = realloc(NULL, 10);
	CHECK(p != NULL);
	fill(p, 0, 10);

	size_t have = 10;
	for (size_t i = 0; i < sizeof steps / sizeof steps[0]; i++) {
		size_t n = steps[i];
		unsigned char *q = realloc(p, n);
		CHECK(q != NULL);
		CHECK(((uintptr_t)q & 15) == 0);
		CHECK(malloc_usable_size(q) >= n);
		size_t kept = have < n ? have : n;
		CHECK(damage(q, kept) == 0);
		fill(q, kept, n);
		p = q;
		have = n;
	}
	free(p);

	/*
	 * realloc(p, 0) follows musl: it returns a live, unique, non-null
	 * pointer, which is then freed.
	 */
	p = malloc(100);
	unsigned char *zero = realloc(p, 0);
	CHECK(zero != NULL);
	unsigned char *other = malloc(0);
	CHECK(other != zero);
	free(other);
	free(zero);
	p = malloc(1 << 20);
	zero = realloc(p, 0);
	CHECK(zero != NULL);
	free(zero);

	/* A large block grows many times over without losing its contents. */
	p = malloc(200000);
	fill(p, 0, 200000);
	have = 200000;
	for (int i = 0; i < 6; i++) {
		unsigned char *q = realloc(p, have * 2);
		CHECK(q != NULL);
		CHECK(damage(q, have) == 0);
		fill(q, have, have * 2);
		p = q;
		have *= 2;
	}
	free(p);

	/*
	 * A request too large fails with ENOMEM and leaves the block alone.
	 * The compiler assumes realloc freed the block, so the calls go
	 * through a pointer it cannot see through.
	 */
	static void *(*volatile realloc_p)(void *, size_t) = realloc;
	p = malloc(50);
	fill(p, 0, 50);
	errno = 0;
	CHECK(realloc_p(p, SIZE_MAX - 8) == NULL);
	CHECK(errno == ENOMEM);
	CHECK(damage(p, 50) == 0);
	p = realloc_p(p, 1 << 20);
	CHECK(p != NULL);
	errno = 0;
	CHECK(realloc_p(p, (size_t)1 << 46) == NULL);
	CHECK(errno == ENOMEM);
	CHECK(damage(p, 50) == 0);
	free(p);

	/* reallocarray resizes by count times size. */
	int *ints = reallocarray(NULL, 10, sizeof(int));
	CHECK(ints != NULL);
	for (int i = 0; i < 10; i++)
		ints[i] = i * i;
	ints = reallocarray(ints, 100000, sizeof(int));
	CHECK(ints != NULL);
	for (int i = 0; i < 10; i++)
		CHECK(ints[i] == i * i);
	free(ints);
	return t_status;
}
