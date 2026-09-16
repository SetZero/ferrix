/*
 * complex.h through the C calling convention: a double complex passed and
 * returned in two SSE registers, a float complex in one, cpow's two
 * arguments in four and two, and the real results in one. Each check is
 * against the bits musl 1.2.5 gives on x86-64 for the same call, and the
 * exceptions it raises, for Annex G's special values and a few ordinary
 * ones. Arguments are built with CMPLX and CMPLXF, which keep a signed zero
 * and an infinity where x + y*I would not.
 *
 * Calls go through volatile pointers, so the compiler neither folds them nor
 * uses its own code, and one call of each kind is direct.
 */

#include <complex.h>
#include <fenv.h>
#include <math.h>
#include <stdint.h>
#include <string.h>
#include "check.h"

static double complex (*volatile csqrt_p)(double complex) = csqrt;
static double complex (*volatile clog_p)(double complex) = clog;
static double complex (*volatile cexp_p)(double complex) = cexp;
static double complex (*volatile casin_p)(double complex) = casin;
static double complex (*volatile cacos_p)(double complex) = cacos;
static double complex (*volatile catanh_p)(double complex) = catanh;
static double complex (*volatile ctanh_p)(double complex) = ctanh;
static double complex (*volatile ccosh_p)(double complex) = ccosh;
static double complex (*volatile cproj_p)(double complex) = cproj;
static double complex (*volatile conj_p)(double complex) = conj;
static double complex (*volatile cpow_p)(double complex, double complex) = cpow;
static double (*volatile cabs_p)(double complex) = cabs;
static double (*volatile carg_p)(double complex) = carg;
static double (*volatile creal_p)(double complex) = (creal);
static double (*volatile cimag_p)(double complex) = (cimag);
static float complex (*volatile csqrtf_p)(float complex) = csqrtf;
static float complex (*volatile clogf_p)(float complex) = clogf;
static float complex (*volatile csinf_p)(float complex) = csinf;
static float complex (*volatile catanf_p)(float complex) = catanf;
static float complex (*volatile cpowf_p)(float complex, float complex) = cpowf;
static float (*volatile cabsf_p)(float complex) = cabsf;
static float (*volatile cimagf_p)(float complex) = (cimagf);

/* Whether the parts of z have these bits. */
static int same(double complex z, uint64_t re, uint64_t im)
{
	double p[2];
	uint64_t b[2];
	memcpy(p, &z, sizeof p);
	memcpy(b, p, sizeof b);
	return b[0] == re && b[1] == im;
}

static int samef(float complex z, uint32_t re, uint32_t im)
{
	float p[2];
	uint32_t b[2];
	memcpy(p, &z, sizeof p);
	memcpy(b, p, sizeof b);
	return b[0] == re && b[1] == im;
}

static int bits(double x, uint64_t want)
{
	uint64_t b;
	memcpy(&b, &x, sizeof b);
	return b == want;
}

static int bitsf(float x, uint32_t want)
{
	uint32_t b;
	memcpy(&b, &x, sizeof b);
	return b == want;
}

static int raised(int want)
{
	int got = fetestexcept(FE_ALL_EXCEPT);
	feclearexcept(FE_ALL_EXCEPT);
	return got == want;
}

int main(void)
{
	double complex z;
	float complex zf;

	/* CMPLX keeps what arithmetic would lose. */
	z = CMPLX(-0.0, INFINITY);
	CHECK(same(z, 0x8000000000000000, 0x7ff0000000000000));
	zf = CMPLXF(-2.0f, -0.0f);
	CHECK(samef(zf, 0xc0000000, 0x80000000));
	CHECK(bits(creal_p(CMPLX(1.5, -0.0)), 0x3ff8000000000000));
	CHECK(bits(cimag_p(CMPLX(1.5, -0.0)), 0x8000000000000000));
	CHECK(bitsf(cimagf_p(CMPLXF(1.5f, -2.0f)), 0xc0000000));
	CHECK(same(conj_p(CMPLX(1, 2)), 0x3ff0000000000000, 0xc000000000000000));
	feclearexcept(FE_ALL_EXCEPT);

	/* Branch cuts: the sign of a zero imaginary part picks the side. */
	CHECK(same(csqrt_p(CMPLX(-2, 0.0)), 0x0, 0x3ff6a09e667f3bcd));
	CHECK(raised(FE_INEXACT));
	CHECK(same(csqrt_p(CMPLX(-2, -0.0)), 0x0, 0xbff6a09e667f3bcd));
	CHECK(raised(FE_INEXACT));
	CHECK(same(csqrt_p(CMPLX(3, 4)), 0x4000000000000000, 0x3ff0000000000000));
	CHECK(raised(0));
	CHECK(same(clog_p(CMPLX(-2, 0.0)), 0x3fe62e42fefa39ef, 0x400921fb54442d18));
	CHECK(raised(FE_INEXACT));
	CHECK(same(clog_p(CMPLX(-2, -0.0)), 0x3fe62e42fefa39ef, 0xc00921fb54442d18));
	CHECK(raised(FE_INEXACT));
	CHECK(samef(csqrtf_p(CMPLXF(-2, -0.0f)), 0x0, 0xbfb504f3));
	CHECK(raised(FE_INEXACT));
	CHECK(samef(clogf_p(CMPLXF(-2, 0.0f)), 0x3f317218, 0x40490fdb));
	CHECK(raised(FE_INEXACT));

	/* Poles and infinities. */
	CHECK(same(clog_p(CMPLX(-0.0, -0.0)), 0xfff0000000000000, 0xc00921fb54442d18));
	CHECK(raised(FE_DIVBYZERO));
	CHECK(same(catanh_p(CMPLX(1, 0)), 0x7ff0000000000000, 0x0));
	CHECK(raised(FE_DIVBYZERO | FE_INEXACT));
	CHECK(same(cexp_p(CMPLX(-INFINITY, -INFINITY)), 0x0, 0x0));
	CHECK(raised(0));
	CHECK(same(ctanh_p(CMPLX(-INFINITY, -INFINITY)), 0xbff0000000000000, 0x8000000000000000));
	CHECK(raised(0));
	CHECK(same(ccosh_p(CMPLX(-INFINITY, -INFINITY)), 0x7ff0000000000000, 0xfff8000000000000));
	CHECK(raised(FE_INVALID));
	CHECK(same(cproj_p(CMPLX(NAN, -INFINITY)), 0x7ff0000000000000, 0x8000000000000000));
	CHECK(raised(0));
	CHECK(bits(cabs_p(CMPLX(NAN, INFINITY)), 0x7ff0000000000000));
	CHECK(bits(carg_p(CMPLX(-INFINITY, -0.0)), 0xc00921fb54442d18));
	CHECK(raised(0));

	/* Ordinary values. */
	CHECK(same(cacos_p(CMPLX(-2, 0.0)), 0x400921fb54442d18, 0xbff5124271980433));
	CHECK(same(casin_p(CMPLX(0.5, -0.25)), 0x3fe00d2e0286798e, 0xbfd202649ab30091));
	CHECK(bitsf(cabsf_p(CMPLXF(3, 4)), 0x40a00000));
	CHECK(samef(catanf_p(CMPLXF(0.5f, -0.25f)), 0x3ef7f035, 0xbe4d6694));
	CHECK(samef(csinf_p(CMPLXF(1, 1)), 0x3fa633db, 0x3f228cfe));
	feclearexcept(FE_ALL_EXCEPT);

	/* cpow: z in the first two registers, c in the next two; with an
	 * infinite c the product's recovery gives exp(inf + inf i). */
	CHECK(same(cpow_p(CMPLX(2, 0), CMPLX(INFINITY, INFINITY)),
		   0x7ff0000000000000, 0xfff8000000000000));
	CHECK(raised(FE_INVALID | FE_INEXACT));
	CHECK(same(cpow_p(CMPLX(0, 0), CMPLX(2, 0)), 0x0, 0x0));
	CHECK(raised(FE_INVALID | FE_DIVBYZERO));
	CHECK(samef(cpowf_p(CMPLXF(2, 0), CMPLXF(INFINITY, INFINITY)), 0x7f800000, 0xffc00000));
	CHECK(raised(FE_INVALID | FE_INEXACT));

	/* Rounding downward, and direct calls. */
	volatile double half = 0.5, quarter = -0.25;
	fesetround(FE_DOWNWARD);
	z = casin(CMPLX(half, quarter));
	fesetround(FE_TONEAREST);
	CHECK(same(z, 0x3fe00d2e0286798e, 0xbfd202649ab3008a));
	zf = csqrtf(CMPLXF((float)half, (float)quarter));
	CHECK(samef(zf, 0x3f3a48cd, 0xbe2fe732));
	CHECK(bits(cabs(CMPLX(half, quarter)), 0x3fe1e3779b97f4a8));

	return t_status;
}
