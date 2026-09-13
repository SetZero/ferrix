/*
 * strtold returns an x87 long double in st(0), through an assembly shim. This
 * checks the bytes against exact long double literals, that the x87 stack is
 * left balanced call after call, and that double results still come back in
 * xmm0 between long double calls.
 */

#include <errno.h>
#include <float.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

static long double (*volatile strtold_p)(const char *, char **) = strtold;
static double (*volatile strtod_p)(const char *, char **) = strtod;

/* The ten bytes of an x87 long double are equal. */
static int same(long double a, long double b)
{
	return memcmp(&a, &b, 10) == 0;
}

static const struct {
	const char *s;
	long double want;
} t[] = {
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
};

int main(void)
{
	char *end;

	CHECK(LDBL_MANT_DIG == 64);
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

	/* The explicit integer bit and the default NaN's quiet bit. */
	unsigned char bytes[10];
	long double nan = strtold_p("-nan(1)", &end);
	memcpy(bytes, &nan, 10);
	CHECK(bytes[7] == 0xc0 && bytes[8] == 0xff && bytes[9] == 0xff);
	CHECK(*end == 0);
	return t_status;
}
