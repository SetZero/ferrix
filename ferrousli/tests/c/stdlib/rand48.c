/*
 * The rand48 family against a reference written from POSIX's description of
 * the 48-bit linear congruential generator, and rand, srand and rand_r
 * against musl's.
 */

#define _XOPEN_SOURCE 700
#include <stdint.h>
#include <stdlib.h>
#include "check.h"

static const uint64_t MASK = (1ULL << 48) - 1;
static uint64_t X, A = 0x5DEECE66DULL, C = 0xB;

static uint64_t step(void)
{
	X = (A * X + C) & MASK;
	return X;
}

static uint64_t join(const unsigned short *w)
{
	return w[0] | (uint64_t)w[1] << 16 | (uint64_t)w[2] << 32;
}

int main(void)
{
	srand48(0x7654321);
	X = 0x330E | (uint64_t)0x7654321 << 16;
	for (int i = 0; i < 1000; i++) {
		CHECK(lrand48() == (long)(step() >> 17));
		CHECK(mrand48() == (long)(int32_t)(step() >> 16));
		CHECK(drand48() == (double)step() / (double)(1ULL << 48));
	}
	/* srand48 uses only the low 32 bits of its argument. */
	srand48(0x100000001L);
	X = 0x330E | 1ULL << 16;
	CHECK(lrand48() == (long)(step() >> 17));

	unsigned short seed[3] = { 0x1234, 0x5678, 0x9abc };
	unsigned short *old = seed48(seed);
	CHECK(join(old) == X);
	X = join(seed);
	CHECK(mrand48() == (long)(int32_t)(step() >> 16));

	unsigned short xsubi[3] = { 1, 2, 3 };
	uint64_t mine = join(xsubi);
	uint64_t global = X;
	for (int i = 0; i < 100; i++) {
		X = mine;
		uint64_t want = step();
		mine = X;
		switch (i % 3) {
		case 0: CHECK(nrand48(xsubi) == (long)(want >> 17)); break;
		case 1: CHECK(jrand48(xsubi) == (long)(int32_t)(want >> 16)); break;
		default: CHECK(erand48(xsubi) == (double)want / (double)(1ULL << 48)); break;
		}
		CHECK(join(xsubi) == mine);
	}
	X = global;
	/* The caller's X did not touch the global one. */
	CHECK(lrand48() == (long)(step() >> 17));

	/* lcong48 sets X, a and c together. */
	unsigned short param[7] = { 5, 0, 0, 3, 0, 0, 7 };
	lcong48(param);
	X = 5; A = 3; C = 7;
	CHECK(lrand48() == (long)(step() >> 17));
	uint64_t before = join(xsubi);
	CHECK(jrand48(xsubi) == (long)(int32_t)(((3 * before + 7) & MASK) >> 16));
	/* POSIX: srand48 and seed48 restore the standard a and c. */
	srand48(1);
	X = 0x330E | 1ULL << 16; A = 0x5DEECE66DULL; C = 0xB;
	CHECK(lrand48() == (long)(step() >> 17));

	/* rand: musl's 64-bit LCG, top 31 bits. */
	uint64_t r = 0;
	srand(1);
	for (int i = 0; i < 100; i++) {
		r = 6364136223846793005ULL * r + 1;
		CHECK(rand() == (int)(r >> 33));
	}
	srand(1);
	int first = rand();
	srand(1);
	CHECK(rand() == first);

	unsigned s1 = 12345, s2 = 12345;
	for (int i = 0; i < 100; i++) {
		int v = rand_r(&s1);
		CHECK(v >= 0 && v <= RAND_MAX);
		CHECK(v == rand_r(&s2));
		CHECK(s1 == s2);
	}
	CHECK(s1 != 12345);
	return t_status;
}
