/*
 * The `long double` functions the x87 computes directly, called through
 * math.h as C calls them: the manipulation, rounding and remainder families.
 *
 * This is the test of the calling convention as much as of the arithmetic. A
 * `long double` argument travels in the caller's stack frame and a result
 * comes back in st(0), which no Rust signature can say, so every one of these
 * is reached through a naked shim. A wrong stack offset in a shim shows up
 * here as a wrong answer or a fault, and nowhere in the Rust unit tests,
 * which call the Rust bodies directly.
 *
 * Every value below is exact: each function either moves bits about or is one
 * x87 instruction that rounds correctly, so the answers are what the standard
 * says and not a library's choice. Calls go through volatile pointers, so the
 * compiler can neither fold them nor use its own code.
 */

#include <fenv.h>
#include <float.h>
#include <limits.h>
#include <math.h>
#include <string.h>
#include "check.h"

static long double (*volatile fabsl_p)(long double) = fabsl;
static long double (*volatile copysignl_p)(long double, long double) = copysignl;
static long double (*volatile fmaxl_p)(long double, long double) = fmaxl;
static long double (*volatile fminl_p)(long double, long double) = fminl;
static long double (*volatile fdiml_p)(long double, long double) = fdiml;
static long double (*volatile sqrtl_p)(long double) = sqrtl;
static long double (*volatile ceill_p)(long double) = ceill;
static long double (*volatile floorl_p)(long double) = floorl;
static long double (*volatile truncl_p)(long double) = truncl;
static long double (*volatile roundl_p)(long double) = roundl;
static long double (*volatile rintl_p)(long double) = rintl;
static long double (*volatile nearbyintl_p)(long double) = nearbyintl;
static long (*volatile lrintl_p)(long double) = lrintl;
static long long (*volatile llrintl_p)(long double) = llrintl;
static long (*volatile lroundl_p)(long double) = lroundl;
static long long (*volatile llroundl_p)(long double) = llroundl;
static long double (*volatile fmodl_p)(long double, long double) = fmodl;
static long double (*volatile remainderl_p)(long double, long double) = remainderl;
static long double (*volatile remquol_p)(long double, long double, int *) = remquol;
static long double (*volatile scalbnl_p)(long double, int) = scalbnl;
static long double (*volatile ldexpl_p)(long double, int) = ldexpl;
static long double (*volatile scalblnl_p)(long double, long) = scalblnl;
static long double (*volatile frexpl_p)(long double, int *) = frexpl;
static long double (*volatile logbl_p)(long double) = logbl;
static int (*volatile ilogbl_p)(long double) = ilogbl;
static long double (*volatile modfl_p)(long double, long double *) = modfl;
static long double (*volatile nextafterl_p)(long double, long double) = nextafterl;
static long double (*volatile nexttowardl_p)(long double, long double) = nexttowardl;
static double (*volatile nexttoward_p)(double, long double) = nexttoward;
static float (*volatile nexttowardf_p)(float, long double) = nexttowardf;
static long double (*volatile nanl_p)(const char *) = nanl;

/* The same ten bytes, which tells -0 from 0 and one NaN from another. */
static int same(long double a, long double b)
{
	return memcmp(&a, &b, 10) == 0;
}

int main(void)
{
	int e;
	long double integral;

	/* The type this is written for: anything else and the constants
	 * below are not the values they are named as. */
	CHECK(LDBL_MANT_DIG == 64);
	CHECK(LDBL_MAX_EXP == 16384);

	/* Signs, taken and given. */
	CHECK(same(fabsl_p(-2.5L), 2.5L));
	CHECK(same(fabsl_p(-0.0L), 0.0L));
	CHECK(same(copysignl_p(2.5L, -1.0L), -2.5L));
	CHECK(same(copysignl_p(-2.5L, 1.0L), 2.5L));
	CHECK(same(copysignl_p(0.0L, -1.0L), -0.0L));

	/* The extremes, where a number beats a NaN and +0 beats -0. */
	CHECK(same(fmaxl_p(1.0L, 2.0L), 2.0L));
	CHECK(same(fminl_p(1.0L, 2.0L), 1.0L));
	CHECK(same(fmaxl_p(NAN, 2.0L), 2.0L));
	CHECK(same(fminl_p(2.0L, NAN), 2.0L));
	CHECK(same(fmaxl_p(-0.0L, 0.0L), 0.0L));
	CHECK(same(fminl_p(0.0L, -0.0L), -0.0L));
	CHECK(same(fdiml_p(5.0L, 2.0L), 3.0L));
	CHECK(same(fdiml_p(2.0L, 5.0L), 0.0L));
	CHECK(isnan(fdiml_p(NAN, 1.0L)));

	/* One x87 instruction each, correctly rounded. */
	CHECK(same(sqrtl_p(0x1p+8L), 16.0L));
	CHECK(same(sqrtl_p(-0.0L), -0.0L));
	CHECK(isnan(sqrtl_p(-1.0L)));

	/* The named directions, which no rounding mode changes. */
	CHECK(same(ceill_p(2.5L), 3.0L));
	CHECK(same(ceill_p(-2.5L), -2.0L));
	CHECK(same(floorl_p(2.5L), 2.0L));
	CHECK(same(floorl_p(-2.5L), -3.0L));
	CHECK(same(truncl_p(2.5L), 2.0L));
	CHECK(same(truncl_p(-2.5L), -2.0L));
	CHECK(same(roundl_p(2.5L), 3.0L));
	CHECK(same(roundl_p(-2.5L), -3.0L));
	CHECK(same(roundl_p(0.4L), 0.0L));
	CHECK(same(roundl_p(-0.4L), -0.0L));

	/* And the mode's own, which does change. */
	CHECK(fesetround(FE_TONEAREST) == 0);
	CHECK(same(rintl_p(2.5L), 2.0L));
	CHECK(same(nearbyintl_p(2.5L), 2.0L));
	CHECK(lrintl_p(2.5L) == 2);
	CHECK(llrintl_p(-2.5L) == -2);
	CHECK(fesetround(FE_UPWARD) == 0);
	CHECK(same(rintl_p(2.5L), 3.0L));
	CHECK(lrintl_p(2.5L) == 3);
	/* round and lround take halves away from zero in every mode. */
	CHECK(same(roundl_p(-2.5L), -3.0L));
	CHECK(lroundl_p(-2.5L) == -3);
	CHECK(fesetround(FE_TONEAREST) == 0);
	CHECK(lroundl_p(2.5L) == 3);
	CHECK(llroundl_p(0.5L) == 1);
	CHECK(llroundl_p(-0.4L) == 0);

	/* nearbyint leaves the inexact flag as it found it; rint raises it. */
	CHECK(feclearexcept(FE_ALL_EXCEPT) == 0);
	(void)nearbyintl_p(2.5L);
	CHECK(fetestexcept(FE_INEXACT) == 0);
	(void)rintl_p(2.5L);
	CHECK(fetestexcept(FE_INEXACT) != 0);
	CHECK(feclearexcept(FE_ALL_EXCEPT) == 0);

	/* What a division leaves: fmod keeps the dividend's sign, remainder
	 * rounds the quotient to nearest and so can change it. */
	CHECK(same(fmodl_p(7.0L, 2.0L), 1.0L));
	CHECK(same(fmodl_p(-7.0L, 2.0L), -1.0L));
	CHECK(same(fmodl_p(7.0L, -2.0L), 1.0L));
	CHECK(same(remainderl_p(7.0L, 2.0L), -1.0L));
	CHECK(same(remainderl_p(5.0L, 2.0L), 1.0L));
	CHECK(same(remainderl_p(-7.0L, 2.0L), 1.0L));
	CHECK(isnan(fmodl_p(1.0L, 0.0L)));

	e = 0xdead;
	CHECK(same(remquol_p(7.0L, 2.0L, &e), -1.0L));
	CHECK(e == 4);
	CHECK(same(remquol_p(-7.0L, 2.0L, &e), 1.0L));
	CHECK(e == -4);
	CHECK(same(remquol_p(1.0L, 2.0L, &e), 1.0L));
	CHECK(e == 0);
	CHECK(same(remquol_p(13.0L, 2.0L, &e), 1.0L));
	CHECK(e == 6);

	/* Scaling by a power of two, which is exact until it leaves the
	 * range. ldexpl is the same function under its other name. */
	CHECK(same(scalbnl_p(3.0L, 4), 48.0L));
	CHECK(same(scalbnl_p(3.0L, -1), 1.5L));
	CHECK(same(ldexpl_p(3.0L, 4), 48.0L));
	CHECK(same(scalblnl_p(3.0L, 4L), 48.0L));
	CHECK(same(scalbnl_p(0.0L, 100), 0.0L));
	CHECK(scalbnl_p(1.0L, 40000) == INFINITY);
	CHECK(same(scalbnl_p(1.0L, -40000), 0.0L));

	/* Taking one apart, and putting the parts back. */
	e = 0xdead;
	CHECK(same(frexpl_p(3.0L, &e), 0.75L));
	CHECK(e == 2);
	CHECK(same(scalbnl_p(0.75L, 2), 3.0L));
	CHECK(same(frexpl_p(-0.0L, &e), -0.0L));
	CHECK(e == 0);
	/* The smallest subnormal, which frexpl reaches by scaling. */
	CHECK(same(frexpl_p(0x1p-16445L, &e), 0.5L));
	CHECK(e == -16444);
	CHECK(isnan(frexpl_p(NAN, &e)));

	CHECK(ilogbl_p(3.0L) == 1);
	CHECK(ilogbl_p(1.0L) == 0);
	CHECK(ilogbl_p(0.5L) == -1);
	CHECK(ilogbl_p(0x1p-16445L) == -16445);
	CHECK(ilogbl_p(0.0L) == FP_ILOGB0);
	CHECK(ilogbl_p(INFINITY) == INT_MAX);
	CHECK(same(logbl_p(3.0L), 1.0L));
	CHECK(logbl_p(0.0L) == -INFINITY);
	CHECK(logbl_p(INFINITY) == INFINITY);

	integral = NAN;
	CHECK(same(modfl_p(2.5L, &integral), 0.5L));
	CHECK(same(integral, 2.0L));
	CHECK(same(modfl_p(-2.5L, &integral), -0.5L));
	CHECK(same(integral, -2.0L));
	CHECK(same(modfl_p(-4.0L, &integral), -0.0L));
	CHECK(same(integral, -4.0L));
	CHECK(same(modfl_p(0.25L, &integral), 0.25L));
	CHECK(same(integral, 0.0L));

	/* One step to the next value the type can hold. */
	CHECK(same(nextafterl_p(1.0L, 2.0L), 1.0L + LDBL_EPSILON));
	CHECK(same(nextafterl_p(nextafterl_p(1.0L, 2.0L), 0.0L), 1.0L));
	CHECK(same(nextafterl_p(0.0L, -1.0L), -0x1p-16445L));
	CHECK(same(nextafterl_p(1.0L, 1.0L), 1.0L));
	CHECK(same(nexttowardl_p(1.0L, 2.0L), 1.0L + LDBL_EPSILON));
	CHECK(isnan(nextafterl_p(NAN, 1.0L)));
	/* And the two that step in a narrower type toward a long double. */
	CHECK(nexttoward_p(1.0, 2.0L) == 1.0 + DBL_EPSILON);
	CHECK(nexttowardf_p(1.0f, 2.0L) == 1.0f + FLT_EPSILON);
	CHECK(nexttoward_p(1.0, 1.0L) == 1.0);
	/* A long double just above 1 is above 1.0 as a double, so the step
	 * is upward even though the two compare equal as doubles. */
	CHECK(nexttoward_p(1.0, 1.0L + LDBL_EPSILON) == 1.0 + DBL_EPSILON);

	CHECK(isnan(nanl_p("")));

	/* Called directly, as a program calls them: the compiler may inline
	 * or fold these, and the answers must not change. */
	CHECK(fabsl(-2.5L) == 2.5L);
	CHECK(ceill(2.5L) == 3.0L);
	CHECK(sqrtl(16.0L) == 4.0L);
	CHECK(fmodl(7.0L, 2.0L) == 1.0L);

	return t_status;
}
