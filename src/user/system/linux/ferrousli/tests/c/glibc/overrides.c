/*
 * A program that brings its own obstack functions, as git's port links GNU's
 * compat/obstack.o: ferrousli's are weak, so the static link takes the
 * program's and does not stop at two definitions -- which it did, for every
 * program, since this library's other names pull the obstack code in with
 * them. And glibc's reentrant random() through stdlib.h, whose struct and
 * declarations fontconfig needs once it finds the functions.
 */

#define _GNU_SOURCE
#include <stdlib.h>

#include "check.h"

/* The program's own, which must be the one called. */
int _obstack_memory_used(void *h)
{
	(void)h;
	return 4242;
}

int main(void)
{
	int (*volatile used)(void *) = _obstack_memory_used;
	CHECK(used(0) == 4242);

	/* Two generators from one seed agree, and differ from another seed's. */
	static char state_a[64], state_b[64], state_c[64];
	struct random_data a = {0}, b = {0}, c = {0};
	CHECK(initstate_r(7, state_a, sizeof state_a, &a) == 0);
	CHECK(initstate_r(7, state_b, sizeof state_b, &b) == 0);
	CHECK(initstate_r(8, state_c, sizeof state_c, &c) == 0);
	int x = 0, y = 0, z = 0, differ = 0;
	for (int i = 0; i < 8; i++) {
		CHECK(random_r(&a, &x) == 0 && random_r(&b, &y) == 0 && random_r(&c, &z) == 0);
		CHECK(x == y && x >= 0);
		differ |= x != z;
	}
	CHECK(differ);
	CHECK(srandom_r(7, &c) == 0);
	CHECK(setstate_r(state_a, &a) == 0);
	return t_status;
}
