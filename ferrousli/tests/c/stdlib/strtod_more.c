/*
 * Where strtod, strtof and atof end their subject, and when they set errno,
 * through the C calling convention.
 */

#include <errno.h>
#include <float.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

static double (*volatile strtod_p)(const char *, char **) = strtod;
static float (*volatile strtof_p)(const char *, char **) = strtof;
static double (*volatile atof_p)(const char *) = atof;

/* Bitwise equality, so that zeros' signs and NaNs compare. */
#define SAME(a, b) (memcmp(&(a), &(b), sizeof (a)) == 0)

#define EXPECT(f, s, value, consumed, err) do { \
	const char *str_ = (s); \
	char *end_ = 0; \
	__typeof__(value) want_ = (value); \
	errno = 0; \
	__typeof__(value) got_ = (f)(str_, &end_); \
	CHECK(SAME(got_, want_)); \
	CHECK(end_ - str_ == (consumed)); \
	CHECK(errno == (err)); \
} while (0)

int main(void)
{
	EXPECT(strtod_p, "1e", 1.0, 1, 0);
	EXPECT(strtod_p, "1e+", 1.0, 1, 0);
	EXPECT(strtod_p, "  -.5e1x", -5.0, 7, 0);
	EXPECT(strtod_p, "0x", 0.0, 1, 0);
	EXPECT(strtod_p, "-0x.p1", -0.0, 2, 0);
	EXPECT(strtod_p, "0x1p", 1.0, 3, 0);
	EXPECT(strtod_p, "0x1p-", 1.0, 3, 0);
	EXPECT(strtod_p, "0X1.8P1", 3.0, 7, 0);
	EXPECT(strtod_p, "inf", (double)INFINITY, 3, 0);
	EXPECT(strtod_p, "-Infinity!", -(double)INFINITY, 9, 0);
	EXPECT(strtod_p, "infinit", (double)INFINITY, 3, 0);
	EXPECT(strtod_p, "", 0.0, 0, EINVAL);
	EXPECT(strtod_p, " +", 0.0, 0, EINVAL);
	EXPECT(strtod_p, ".", 0.0, 0, EINVAL);
	EXPECT(strtod_p, "in", 0.0, 0, EINVAL);
	EXPECT(strtod_p, "1e309", (double)INFINITY, 5, ERANGE);
	EXPECT(strtod_p, "-1e-400", -0.0, 7, ERANGE);
	EXPECT(strtod_p, "4.9e-324", 0x1p-1074, 8, ERANGE);
	EXPECT(strtod_p, "0x1p-1074", 0x1p-1074, 9, 0);
	EXPECT(strtod_p, "0x1.8p-1074", 0x1p-1073, 11, 0);
	EXPECT(strtod_p, "0x1p-1075", 0.0, 9, ERANGE);
	EXPECT(strtod_p, "0x1p1024", (double)INFINITY, 8, ERANGE);
	EXPECT(strtod_p, "1.7976931348623157e308", DBL_MAX, 22, 0);

	EXPECT(strtof_p, "3.4028236e38", (float)INFINITY, 12, ERANGE);
	EXPECT(strtof_p, "3.4028235e38", FLT_MAX, 12, 0);
	EXPECT(strtof_p, "1e-46", 0.0f, 5, ERANGE);
	EXPECT(strtof_p, "0x1p-149", 0x1p-149f, 8, 0);
	EXPECT(strtof_p, "0.1", 0.1f, 3, 0);
	EXPECT(strtof_p, "16777217", 16777216.0f, 8, 0);

	char *end;
	double n = strtod_p("nan(chars_123)z", &end);
	CHECK(n != n);
	CHECK(*end == 'z');
	n = strtod_p("NAN(", &end);
	CHECK(n != n);
	CHECK(*end == '(');
	n = strtod_p("-nan", &end);
	CHECK(n != n);
	CHECK(((unsigned char *)&n)[7] & 0x80);

	CHECK(atof_p("  -2.5e-3xyz") == -2.5e-3);
	CHECK(atof_p("0x10") == 16.0);
	return t_status;
}
