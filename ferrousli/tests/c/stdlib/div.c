/*
 * abs and div in each width, called through pointers so the compiler cannot
 * fold them. div_t, ldiv_t, lldiv_t and imaxdiv_t come back by value, in rax
 * alone or in rax and rdx, as the SysV ABI returns small structures.
 */

#include <inttypes.h>
#include <limits.h>
#include <stdlib.h>
#include "check.h"

static int (*volatile abs_p)(int) = abs;
static long (*volatile labs_p)(long) = labs;
static long long (*volatile llabs_p)(long long) = llabs;
static intmax_t (*volatile imaxabs_p)(intmax_t) = imaxabs;
static div_t (*volatile div_p)(int, int) = div;
static ldiv_t (*volatile ldiv_p)(long, long) = ldiv;
static lldiv_t (*volatile lldiv_p)(long long, long long) = lldiv;
static imaxdiv_t (*volatile imaxdiv_p)(intmax_t, intmax_t) = imaxdiv;

int main(void)
{
	CHECK(sizeof(div_t) == 8);
	CHECK(sizeof(ldiv_t) == 2 * sizeof(long));
	CHECK(sizeof(lldiv_t) == 16);
	CHECK(sizeof(imaxdiv_t) == 16);

	CHECK(abs_p(-5) == 5);
	CHECK(abs_p(INT_MIN + 1) == INT_MAX);
	CHECK(labs_p(LONG_MIN + 1) == LONG_MAX);
	CHECK(llabs_p(-3) == 3);
	CHECK(imaxabs_p(INTMAX_MIN + 1) == INTMAX_MAX);

	div_t d = div_p(7, -2);
	CHECK(d.quot == -3 && d.rem == 1);
	d = div_p(-7, 2);
	CHECK(d.quot == -3 && d.rem == -1);
	d = div_p(INT_MIN, 3);
	CHECK(d.quot == INT_MIN / 3 && d.rem == INT_MIN % 3);

	/* A divisor of 2^20 on a 32-bit target, where a long is 32 bits. */
	enum { SHIFT = sizeof(long) == 8 ? 40 : 20 };
	ldiv_t ld = ldiv_p(LONG_MAX, 1L << SHIFT);
	CHECK(ld.quot == LONG_MAX >> SHIFT && ld.rem == (LONG_MAX & ((1L << SHIFT) - 1)));
	ld = ldiv_p(-9, 4);
	CHECK(ld.quot == -2 && ld.rem == -1);

	lldiv_t lld = lldiv_p(LLONG_MIN, 7);
	CHECK(lld.quot == LLONG_MIN / 7 && lld.rem == LLONG_MIN % 7);

	imaxdiv_t id = imaxdiv_p(-1000000000000LL, -999);
	CHECK(id.quot == 1001001001 && id.rem == -1);
	return t_status;
}
