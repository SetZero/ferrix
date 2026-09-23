/*
 * aligned_alloc, posix_memalign, memalign, valloc and pvalloc at every
 * power-of-two alignment from 1 to 65536, and beyond a page, with the results
 * written whole, freed by free and resized by realloc. And the EINVAL cases.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <malloc.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

/* glibc exports it, and musl's headers do not declare it. */
void *pvalloc(size_t);

static const size_t sizes[] = { 0, 1, 16, 100, 4000, 131056, 131057, 300000 };
#define SIZES (sizeof sizes / sizeof sizes[0])

static void use(unsigned char *p, size_t align, size_t n)
{
	CHECK(p != NULL);
	if (!p)
		return;
	CHECK(((uintptr_t)p & (align - 1)) == 0);
	CHECK(((uintptr_t)p & 15) == 0);
	size_t usable = malloc_usable_size(p);
	CHECK(usable >= n);
	for (size_t at = 0; at < usable; at++)
		p[at] = (unsigned char)(at ^ align);
}

static size_t damage(const unsigned char *p, size_t align, size_t n)
{
	size_t bad = 0;
	for (size_t at = 0; at < n; at++)
		bad += p[at] != (unsigned char)(at ^ align);
	return bad;
}

int main(void)
{
	void *held[4 * 18 * SIZES];
	size_t count = 0;

	for (size_t align = 1; align <= 1 << 20; align <<= 1) {
		for (size_t i = 0; i < SIZES; i++) {
			size_t n = sizes[i];
			unsigned char *p;

			p = aligned_alloc(align, n);
			use(p, align, n);
			held[count++] = p;

			p = memalign(align, n);
			use(p, align, n);
			held[count++] = p;

			if (align >= sizeof(void *)) {
				p = NULL;
				CHECK(posix_memalign((void **)&p, align, n) == 0);
				use(p, align, n);
				held[count++] = p;
			}

			/* Resizing keeps the contents, if not the alignment. */
			p = aligned_alloc(align, n + 64);
			use(p, align, n + 64);
			unsigned char *q = realloc(p, n + 100000);
			CHECK(q != NULL);
			CHECK(damage(q, align, n + 64) == 0);
			free(q);
		}
	}
	/* Every pointer survives the others being written, then frees. */
	for (size_t i = 0; i < count; i++)
		free(held[i]);

	unsigned char *p = valloc(10);
	use(p, 4096, 10);
	free(p);
	p = pvalloc(10);
	use(p, 4096, 4096);
	free(p);
	p = pvalloc(0);
	use(p, 4096, 4096);
	CHECK(malloc_usable_size(p) >= 4096);
	free(p);

	/* EINVAL from posix_memalign leaves errno and the pointer alone. */
	static const size_t bad_posix[] = { 0, 1, 2, 3, sizeof(void *) / 2, 6, 12, 24, 100, 4097,
					    SIZE_MAX };
	for (size_t i = 0; i < sizeof bad_posix / sizeof bad_posix[0]; i++) {
		void *out = &p;
		errno = 0;
		CHECK(posix_memalign(&out, bad_posix[i], 16) == EINVAL);
		CHECK(errno == 0);
		CHECK(out == &p);
	}
	/*
	 * aligned_alloc and memalign set errno instead. At -O2 the compiler
	 * deletes an allocation whose result is only compared with NULL, so
	 * the calls that must fail go through pointers it cannot see through.
	 */
	static void *(*volatile aligned_alloc_p)(size_t, size_t) = aligned_alloc;
	static void *(*volatile memalign_p)(size_t, size_t) = memalign;
	static void *(*volatile pvalloc_p)(size_t) = pvalloc;
	static const size_t bad_align[] = { 3, 6, 12, 24, 100, 4097, SIZE_MAX };
	for (size_t i = 0; i < sizeof bad_align / sizeof bad_align[0]; i++) {
		errno = 0;
		CHECK(aligned_alloc_p(bad_align[i], 16) == NULL);
		CHECK(errno == EINVAL);
		errno = 0;
		CHECK(memalign_p(bad_align[i], 16) == NULL);
		CHECK(errno == EINVAL);
	}

	/* Too large for any alignment, and an alignment too large for any size. */
	void *out = NULL;
	errno = 0;
	CHECK(posix_memalign(&out, 4096, SIZE_MAX - 100) == ENOMEM);
	CHECK(out == NULL);
	errno = 0;
	CHECK(aligned_alloc_p((size_t)1 << (sizeof(size_t) * 8 - 1), 1) == NULL);
	CHECK(errno == ENOMEM);
	errno = 0;
	CHECK(pvalloc_p(SIZE_MAX - 10) == NULL);
	CHECK(errno == ENOMEM);
	return t_status;
}
