/*
 * sin, cos, exp, log, pow and atan2 through math.h, against the bits musl
 * 1.2.5 gives on x86-64 for the same calls, and the special cases C names.
 *
 * Most calls go through volatile pointers, so the compiler can neither fold
 * them nor replace them with its own code; the last few are direct calls, as
 * programs make them. None of the functions sets errno: math_errhandling is
 * MATH_ERREXCEPT, as in musl.
 */

#include <errno.h>
#include <math.h>
#include <string.h>
#include "check.h"

static double (*volatile sin_p)(double) = sin;
static double (*volatile cos_p)(double) = cos;
static double (*volatile exp_p)(double) = exp;
static double (*volatile log_p)(double) = log;
static double (*volatile pow_p)(double, double) = pow;
static double (*volatile atan2_p)(double, double) = atan2;

/* The same bits, which tells -0 from 0. */
static int same(double a, double b)
{
	return memcmp(&a, &b, sizeof a) == 0;
}

int main(void)
{
	CHECK(math_errhandling == MATH_ERREXCEPT);
	errno = 0;

	CHECK(same(sin_p(1), 0x1.aed548f090ceep-1));
	CHECK(same(sin_p(-3), -0x1.210386db6d55bp-3));
	CHECK(same(sin_p(1e22), -0x1.b453ab76bf397p-1));
	CHECK(same(sin_p(0x1p-1030), 0x1p-1030));
	CHECK(same(sin_p(-0.0), -0.0));
	CHECK(isnan(sin_p(INFINITY)));

	CHECK(same(cos_p(1), 0x1.14a280fb5068cp-1));
	CHECK(same(cos_p(10), -0x1.ad9ac890c6b1fp-1));
	CHECK(same(cos_p(0x1.921fb54442d18p+0), 0x1.1a62633145c07p-54));
	CHECK(same(cos_p(-0.0), 1.0));
	CHECK(isnan(cos_p(-INFINITY)));

	CHECK(same(exp_p(1), 0x1.5bf0a8b145769p+1));
	CHECK(same(exp_p(-1), 0x1.78b56362cef38p-2));
	CHECK(same(exp_p(709), 0x1.d422d2be5dc9bp+1022));
	CHECK(same(exp_p(-740), 0x1.54p-1068));
	CHECK(same(exp_p(-745), 0x1p-1074));
	CHECK(same(exp_p(0), 1.0));
	CHECK(same(exp_p(1000), INFINITY));
	CHECK(same(exp_p(-1000), 0.0));
	CHECK(same(exp_p(-INFINITY), 0.0));
	CHECK(isnan(exp_p(NAN)));

	CHECK(same(log_p(2), 0x1.62e42fefa39efp-1));
	CHECK(same(log_p(10), 0x1.26bb1bbb55516p+1));
	CHECK(same(log_p(0x1p-1074), -0x1.74385446d71c3p+9));
	CHECK(same(log_p(0.9375), -0x1.08598b59e3a07p-4));
	CHECK(same(log_p(1), 0.0));
	CHECK(same(log_p(-0.0), -INFINITY));
	CHECK(same(log_p(INFINITY), INFINITY));
	CHECK(isnan(log_p(-1)));

	CHECK(same(pow_p(2, 0.5), 0x1.6a09e667f3bcdp+0));
	CHECK(same(pow_p(10, -3), 0x1.0624dd2f1a9fcp-10));
	CHECK(same(pow_p(-3, 3), -27.0));
	CHECK(same(pow_p(1.5, -1000), 0x1.06bdc6f923b3bp-585));
	CHECK(same(pow_p(0.5, 1074), 0x1p-1074));
	CHECK(same(pow_p(NAN, 0), 1.0));
	CHECK(same(pow_p(1, NAN), 1.0));
	CHECK(same(pow_p(-0.0, -3), -INFINITY));
	CHECK(same(pow_p(10, 400), INFINITY));
	CHECK(same(pow_p(-10, -401), -0.0));
	CHECK(isnan(pow_p(-2, 0.5)));

	CHECK(same(atan2_p(1, 1), 0x1.921fb54442d18p-1));
	CHECK(same(atan2_p(-1, -1), -0x1.2d97c7f3321d2p+1));
	CHECK(same(atan2_p(1, -1e300), 0x1.921fb54442d18p+1));
	CHECK(same(atan2_p(3, 4), 0x1.4978fa3269ee1p-1));
	CHECK(same(atan2_p(-INFINITY, INFINITY), -0x1.921fb54442d18p-1));
	CHECK(same(atan2_p(-0.0, 1), -0.0));
	CHECK(same(atan2_p(0.0, -0.0), 0x1.921fb54442d18p+1));
	CHECK(isnan(atan2_p(NAN, 1)));

	volatile double one = 1, two = 2, half = 0.5;
	CHECK(same(sin(one), 0x1.aed548f090ceep-1));
	CHECK(same(cos(one), 0x1.14a280fb5068cp-1));
	CHECK(same(exp(one), 0x1.5bf0a8b145769p+1));
	CHECK(same(log(two), 0x1.62e42fefa39efp-1));
	CHECK(same(pow(two, half), 0x1.6a09e667f3bcdp+0));
	CHECK(same(atan2(one, one), 0x1.921fb54442d18p-1));

	CHECK(errno == 0);
	return t_status;
}
