/*
 * strtold returns a long double through an assembly shim: an x87 one in st(0)
 * on x86-64, a binary128 one in q0 on AArch64. On ARMv7-A a long double is a
 * double, returned in d0 as one. This checks the bytes against exact long
 * double literals, that the x87 stack is left balanced call after call, and
 * that double results still come back in their register between long double
 * calls.
 */

#include <errno.h>
#include <float.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

static long double (*volatile strtold_p)(const char *, char **) = strtold;
static double (*volatile strtod_p)(const char *, char **) = strtod;

/* The bytes of a long double are equal: an x87 one's ten, or all. */
static int same(long double a, long double b)
{
	return memcmp(&a, &b, LDBL_MANT_DIG == 64 ? 10 : sizeof a) == 0;
}

static const struct {
	const char *s;
	long double want;
} t[] = {
#if LDBL_MANT_DIG == 64
	{"1", 1.0L},
	{"-2.5", -2.5L},
	{"0.1", 0xc.ccccccccccccccdp-7L},
	{"3.14159265358979323846264338327950288", 0xc.90fdaa22168c235p-2L},
	{"1.18973149535723176502e4932", LDBL_MAX},
	{"3.36210314311209350626e-4932", LDBL_MIN},
	{"3.64519953188247460253e-4951", 0x1p-16445L},
	{"0x1p-16445", 0x1p-16445L},
	{"0x1.fffffffffffffffep16383", LDBL_MAX},
	{"-0", -0.0L},
	{"18446744073709551615", 18446744073709551615.0L},
	{"18446744073709551617", 18446744073709551616.0L},
	{"0x1.00000000000000018p0", 0x1.0000000000000002p0L},
	{"0x1.00000000000000008p0", 1.0L},
#elif LDBL_MANT_DIG == 113
	{"1", 1.0L},
	{"-2.5", -2.5L},
	{"0.1", 0x1.999999999999999999999999999ap-4L},
	{"3.14159265358979323846264338327950288", 0x1.921fb54442d18469898cc51701b8p1L},
	{"1.18973149535723176508575932662800702e4932", LDBL_MAX},
	{"3.36210314311209350626267781732175260e-4932", LDBL_MIN},
	{"6.47517511943802511092443895822764657e-4966", 0x1p-16494L},
	{"0x1p-16494", 0x1p-16494L},
	{"0x1.ffffffffffffffffffffffffffffp16383", LDBL_MAX},
	{"-0", -0.0L},
	{"10384593717069655257060992658440191", 10384593717069655257060992658440191.0L},
	{"10384593717069655257060992658440193", 10384593717069655257060992658440192.0L},
	{"0x1.00000000000000000000000000018p0", 0x1.0000000000000000000000000002p0L},
	{"0x1.00000000000000000000000000008p0", 1.0L},
#else
	{"1", 1.0L},
	{"-2.5", -2.5L},
	{"0.1", 0x1.999999999999ap-4L},
	{"3.14159265358979323846264338327950288", 0x1.921fb54442d18p1L},
	{"1.7976931348623157e308", LDBL_MAX},
	{"2.2250738585072014e-308", LDBL_MIN},
	{"4.9406564584124654e-324", 0x1p-1074L},
	{"0x1p-1074", 0x1p-1074L},
	{"0x1.fffffffffffffp1023", LDBL_MAX},
	{"-0", -0.0L},
	{"9007199254740991", 9007199254740991.0L},
	{"9007199254740993", 9007199254740992.0L},
	{"0x1.00000000000018p0", 0x1.0000000000002p0L},
	{"0x1.00000000000008p0", 1.0L},
#endif
};

int main(void)
{
	char *end;

	for (size_t i = 0; i < sizeof t / sizeof *t; i++) {
		long double x = strtold_p(t[i].s, &end);
		CHECK(same(x, t[i].want));
		CHECK(*end == 0);
	}

	/* A shim that left a value on the x87 stack would overflow it by the
	 * ninth call and turn the sum into a NaN. */
	long double sum = 0;
	for (int i = 0; i < 100; i++)
		sum += strtold_p("0.5", 0);
	CHECK(sum == 50.0L);

	double d = strtod_p("0.25", 0);
	long double e = strtold_p("0.75", 0);
	double f = strtod_p("0.125", 0);
	CHECK(d + e + f == 1.125L);

	errno = 0;
	long double big = strtold_p("-1e99999x", &end);
	long double minus_infinity = -(long double)INFINITY;
	CHECK(same(big, minus_infinity));
	CHECK(errno == ERANGE);
	CHECK(*end == 'x');

	/* The sign, the exponent of all ones, and the quiet bit -- after the
	   x87 format's explicit integer bit. */
	unsigned char bytes[sizeof(long double)];
	long double nan = strtold_p("-nan(1)", &end);
	memcpy(bytes, &nan, sizeof bytes);
#if LDBL_MANT_DIG == 64
	CHECK(bytes[7] == 0xc0 && bytes[8] == 0xff && bytes[9] == 0xff);
#elif LDBL_MANT_DIG == 113
	CHECK(bytes[15] == 0xff && bytes[14] == 0xff && (bytes[13] & 0x80));
#else
	CHECK(bytes[7] == 0xff && (bytes[6] & 0xf8) == 0xf8);
#endif
	CHECK(*end == 0);
	return t_status;
}
