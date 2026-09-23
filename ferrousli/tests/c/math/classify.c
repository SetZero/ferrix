/*
 * math.h's classification and comparison macros on float, double and long
 * double.
 *
 * fpclassify calls __fpclassify, __fpclassifyf and __fpclassifyl. For long
 * double, isinf, isnan, isnormal, isfinite, isunordered and isless to
 * isgreaterequal call __fpclassifyl too, and signbit calls __signbitl; those
 * two take their argument on the stack on x86-64 and in q0 on AArch64. The
 * x87 encodings with a nonzero exponent and the integer bit clear are NaNs,
 * as in musl, and a pseudo-denormal is normal; binary128 has no such
 * encodings. On ARMv7-A a long double is a double. Some cases are adapted
 * from libc-test's fpclassify.c (MIT).
 */

#include <float.h>
#include <math.h>
#include <string.h>
#include "check.h"

#if LDBL_MANT_DIG == 64
/* The long double with this significand and sign-and-exponent word. */
static long double x87(unsigned long long mantissa, unsigned sign_exponent)
{
	unsigned char bytes[sizeof(long double)] = {0};
	memcpy(bytes, &mantissa, 8);
	bytes[8] = sign_exponent & 0xff;
	bytes[9] = sign_exponent >> 8;
	long double x;
	memcpy(&x, bytes, sizeof x);
	return x;
}

static const struct {
	unsigned long long mantissa;
	unsigned sign_exponent;
	int class;
} long_doubles[] = {
	{0, 0, FP_ZERO},
	{0, 0x8000, FP_ZERO},
	{1, 0, FP_SUBNORMAL},
	{0x7fffffffffffffffull, 0x8000, FP_SUBNORMAL},
	/* A pseudo-denormal. */
	{0x8000000000000000ull, 0, FP_NORMAL},
	/* LDBL_MIN, -1 and LDBL_MAX. */
	{0x8000000000000000ull, 1, FP_NORMAL},
	{0x8000000000000000ull, 0xbfff, FP_NORMAL},
	{0xffffffffffffffffull, 0x7ffe, FP_NORMAL},
	/* An unnormal, a pseudo-infinity and a pseudo-NaN. */
	{0x7fffffffffffffffull, 0x3fff, FP_NAN},
	{0, 0x7fff, FP_NAN},
	{0x4000000000000000ull, 0xffff, FP_NAN},
	{0x8000000000000000ull, 0x7fff, FP_INFINITE},
	{0x8000000000000000ull, 0xffff, FP_INFINITE},
	/* A quiet NaN and a signalling one. */
	{0xc000000000000000ull, 0x7fff, FP_NAN},
	{0x8000000000000001ull, 0xffff, FP_NAN},
};

#define N_LONG (sizeof long_doubles / sizeof *long_doubles)
static long double long_at(size_t i)
{
	return x87(long_doubles[i].mantissa, long_doubles[i].sign_exponent);
}
static int long_class(size_t i) { return long_doubles[i].class; }
static int long_negative(size_t i) { return !!(long_doubles[i].sign_exponent & 0x8000); }
#elif LDBL_MANT_DIG == 113
/* The binary128 long double with these high and low 64 bits. */
static long double quad(unsigned long long high, unsigned long long low)
{
	unsigned char bytes[16];
	long double x;
	memcpy(bytes, &low, 8);
	memcpy(bytes + 8, &high, 8);
	memcpy(&x, bytes, sizeof x);
	return x;
}

static const struct {
	unsigned long long high, low;
	int class;
} long_doubles[] = {
	{0, 0, FP_ZERO},
	{0x8000000000000000ull, 0, FP_ZERO},
	/* The smallest and largest subnormals. */
	{0, 1, FP_SUBNORMAL},
	{0x8000ffffffffffffull, 0xffffffffffffffffull, FP_SUBNORMAL},
	/* LDBL_MIN, -1 and LDBL_MAX. */
	{0x0001000000000000ull, 0, FP_NORMAL},
	{0xbfff000000000000ull, 0, FP_NORMAL},
	{0x7ffeffffffffffffull, 0xffffffffffffffffull, FP_NORMAL},
	{0x7fff000000000000ull, 0, FP_INFINITE},
	{0xffff000000000000ull, 0, FP_INFINITE},
	/* A quiet NaN and a signalling one. */
	{0x7fff800000000000ull, 0, FP_NAN},
	{0xffff000000000000ull, 1, FP_NAN},
};

#define N_LONG (sizeof long_doubles / sizeof *long_doubles)
static long double long_at(size_t i)
{
	return quad(long_doubles[i].high, long_doubles[i].low);
}
static int long_class(size_t i) { return long_doubles[i].class; }
static int long_negative(size_t i) { return !!(long_doubles[i].high >> 63); }
#else
static const struct {
	double x;
	int class;
} long_doubles[] = {
	{0.0, FP_ZERO},
	{-0.0, FP_ZERO},
	{-DBL_TRUE_MIN, FP_SUBNORMAL},
	{DBL_MAX, FP_NORMAL},
	{-INFINITY, FP_INFINITE},
	{NAN, FP_NAN},
};

#define N_LONG (sizeof long_doubles / sizeof *long_doubles)
static long double long_at(size_t i) { return long_doubles[i].x; }
static int long_class(size_t i) { return long_doubles[i].class; }
static int long_negative(size_t i) { return !!signbit(long_doubles[i].x); }
#endif

static const struct {
	double x;
	int class;
} doubles[] = {
	{0.0, FP_ZERO},
	{-0.0, FP_ZERO},
	{DBL_MIN, FP_NORMAL},
	{DBL_MIN / 2, FP_SUBNORMAL},
	{-DBL_TRUE_MIN, FP_SUBNORMAL},
	{1.0, FP_NORMAL},
	{-DBL_MAX, FP_NORMAL},
	{INFINITY, FP_INFINITE},
	{-INFINITY, FP_INFINITE},
	{NAN, FP_NAN},
	{-NAN, FP_NAN},
};

static const struct {
	float x;
	int class;
} floats[] = {
	{0.0f, FP_ZERO},
	{-0.0f, FP_ZERO},
	{FLT_MIN, FP_NORMAL},
	{FLT_MIN / 2, FP_SUBNORMAL},
	{-FLT_TRUE_MIN, FP_SUBNORMAL},
	{1.0f, FP_NORMAL},
	{FLT_MAX, FP_NORMAL},
	{INFINITY, FP_INFINITE},
	{-INFINITY, FP_INFINITE},
	{NAN, FP_NAN},
	{-NAN, FP_NAN},
};

int main(void)
{
	for (size_t i = 0; i < N_LONG; i++) {
		volatile long double x = long_at(i);
		int class = long_class(i);
		CHECK(fpclassify(x) == class);
		CHECK(!!isnan(x) == (class == FP_NAN));
		CHECK(!!isinf(x) == (class == FP_INFINITE));
		CHECK(!!isnormal(x) == (class == FP_NORMAL));
		CHECK(!!isfinite(x) == (class != FP_NAN && class != FP_INFINITE));
		CHECK((signbit(x) != 0) == long_negative(i));
		CHECK(!!isunordered(x, 0.0L) == (class == FP_NAN));
		CHECK(!!isunordered(1.0L, x) == (class == FP_NAN));
	}

	volatile long double one = 1, two = 2, nan = NAN, big = LDBL_MAX;
	volatile long double minus_zero = -0.0L;
#if LDBL_MANT_DIG == 64
	/* An unnormal compares as a NaN. */
	volatile long double unnormal = x87(0x7fffffffffffffffull, 0x3fff);
	CHECK(!isless(unnormal, big));
	CHECK(!isgreater(big, unnormal));
	CHECK(isunordered(one, unnormal));
#endif
	CHECK(isless(one, two));
	CHECK(!isless(two, one));
	CHECK(!isless(nan, one));
	CHECK(!isless(one, nan));
	CHECK(islessequal(one, one));
	CHECK(islessequal(minus_zero, 0.0L));
	CHECK(!islessequal(nan, nan));
	CHECK(islessgreater(one, two));
	CHECK(!islessgreater(one, one));
	CHECK(!islessgreater(nan, one));
	CHECK(isgreater(big, one));
	CHECK(!isgreater(one, big));
	CHECK(!isgreater(nan, one));
	CHECK(isgreaterequal(minus_zero, 0.0L));
	CHECK(!isgreaterequal(one, nan));
	CHECK(isunordered(nan, one));
	CHECK(!isunordered(one, big));
	CHECK(signbit(minus_zero));
	CHECK(signbit(-nan));
	CHECK(!signbit(one));

	for (size_t i = 0; i < sizeof doubles / sizeof *doubles; i++) {
		volatile double x = doubles[i].x;
		int class = doubles[i].class;
		unsigned long long bits;
		memcpy(&bits, &doubles[i].x, sizeof bits);
		CHECK(fpclassify(x) == class);
		CHECK(!!isnan(x) == (class == FP_NAN));
		CHECK(!!isinf(x) == (class == FP_INFINITE));
		CHECK(!!isnormal(x) == (class == FP_NORMAL));
		CHECK(!!isfinite(x) == (class != FP_NAN && class != FP_INFINITE));
		CHECK((signbit(x) != 0) == (int)(bits >> 63));
		CHECK(!!isunordered(x, 0.0) == (class == FP_NAN));
		CHECK(!!isless(x, INFINITY) == (class != FP_NAN && x != INFINITY));
		CHECK(!!isgreaterequal(x, -INFINITY) == (class != FP_NAN));
	}

	for (size_t i = 0; i < sizeof floats / sizeof *floats; i++) {
		volatile float x = floats[i].x;
		int class = floats[i].class;
		unsigned bits;
		memcpy(&bits, &floats[i].x, sizeof bits);
		CHECK(fpclassify(x) == class);
		CHECK(!!isnan(x) == (class == FP_NAN));
		CHECK(!!isinf(x) == (class == FP_INFINITE));
		CHECK(!!isnormal(x) == (class == FP_NORMAL));
		CHECK(!!isfinite(x) == (class != FP_NAN && class != FP_INFINITE));
		CHECK((signbit(x) != 0) == (int)(bits >> 31));
		CHECK(!!isunordered(0.0f, x) == (class == FP_NAN));
		CHECK(!!islessgreater(x, 0.0f) == (class != FP_NAN && class != FP_ZERO));
		CHECK(!!islessequal(x, (float)INFINITY) == (class != FP_NAN));
	}

	return t_status;
}
