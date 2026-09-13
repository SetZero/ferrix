/*
 * Adapted from libc-test's src/functional/strtod_long.c (MIT): 40000 digits,
 * far beyond the 768 that can matter, must still round correctly.
 */

#include <stdlib.h>
#include <string.h>
#include "check.h"

int main(void)
{
	double x, want = .1111111111111111111111;
	char buf[40000];
	char *end;

	memset(buf, '1', sizeof buf);
	buf[0] = '.';
	buf[sizeof buf - 1] = 0;

	x = strtod(buf, &end);
	CHECK(x == want);
	CHECK(end == buf + sizeof buf - 1);

	/* A 1 and 39991 zeros before the point, scaled back down to 1. */
	memset(buf, '0', sizeof buf);
	buf[0] = '1';
	memcpy(buf + sizeof buf - 8, "e-39991", 8);
	CHECK(strtod(buf, 0) == 1.0);
	return t_status;
}
