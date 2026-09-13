/*
 * Adapted from libc-test's src/functional/random.c (MIT): naive statistical
 * checks on random, and initstate and setstate. Its t_error reports became
 * CHECKs.
 */

#define _XOPEN_SOURCE 700
#include <stdlib.h>
#include "check.h"

/* error p ~ 1.6e-6 */
static int chkmissing(long *x)
{
	int d[8] = {0};
	int i;
	for (i = 0; i < 100; i++)
		d[x[i]%8]++;
	for (i = 0; i < 8; i++)
		if (d[i]==0)
			return 1;
	return 0;
}

/* error p ~ 4e-6 */
static int chkrepeat(long *x)
{
	int i, j;
	for (i = 0; i < 100; i++)
		for (j = 0; j < i; j++)
			if (x[i] == x[j])
				return 1;
	return 0;
}

/* error p ~ 1e-6 */
static unsigned orx;
static int chkones(long *x)
{
	int i;
	orx = 0;
	for (i = 0; i < 20; i++)
		orx |= x[i];
	return orx != 0x7fffffff;
}

static void checkseed(unsigned seed, long *x)
{
	int i;
	srandom(seed);
	for (i = 0; i < 100; i++)
		x[i] = random();
	CHECK(!chkmissing(x));
	CHECK(!chkrepeat(x));
	CHECK(!chkones(x));
}

int main(void)
{
	long x[100];
	long y,z;
	int i;
	char state[128];
	char *p;
	char *q;

	for (i = 0; i < 100; i++)
		x[i] = random();
	p = initstate(1, state, sizeof state);
	for (i = 0; i < 100; i++)
		CHECK(x[i] == (y = random()));
	for (i = 0; i < 10; i++) {
		z = random();
		q = setstate(p);
		CHECK(z == (y = random()));
		p = setstate(q);
	}
	srandom(1);
	for (i = 0; i < 100; i++)
		CHECK(x[i] == (y = random()));
	checkseed(0x7fffffff, x);
	for (i = 0; i < 10; i++)
		checkseed(i, x);

	/* Every state size, including the unaligned buffer a char array may be. */
	static char sizes[1 + 300];
	for (size_t size = 8; size <= 300; size += 23) {
		char *old = initstate(42, sizes + 1, size);
		CHECK(old != 0);
		long first = random();
		CHECK(first >= 0 && first <= 0x7fffffff);
		setstate(old);
		old = initstate(42, sizes + 1, size);
		CHECK(random() == first);
		setstate(old);
	}
	CHECK(initstate(1, state, 7) == 0);
	return t_status;
}
