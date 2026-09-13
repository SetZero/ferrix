/*
 * Adapted from libc-test's src/regression/lrand48-signextend.c (MIT):
 * lrand48 should give deterministic results.
 */

#define _XOPEN_SOURCE 700
#include <stdlib.h>
#include "check.h"

int main(void)
{
	CHECK(lrand48() == 0);
	CHECK(lrand48() == 2116118);
	CHECK(lrand48() == 89401895);
	return t_status;
}
