/*
 * GNU's math.h additions Chrome and systemd call, through C's calling
 * convention: sincos and sincosf, exp10 and exp10f, cbrt and cbrtf, and
 * powl, whose long double arguments travel on the stack on x86-64.
 */

#define _GNU_SOURCE
#include <float.h>
#include <math.h>

#include "check.h"

void sincos(double, double *, double *);
void sincosf(float, float *, float *);
double exp10(double);
float exp10f(float);

int main(void)
{
	double s, c;
	float sf, cf;
	volatile double x = 0.5;
	volatile float xf = 0.5f;

	sincos(x, &s, &c);
	CHECK(s == sin(x) && c == cos(x));
	sincosf(xf, &sf, &cf);
	CHECK(sf == sinf(xf) && cf == cosf(xf));
	sincos(0.0, &s, &c);
	CHECK(s == 0.0 && c == 1.0);

	CHECK(exp10(3.0) == 1000.0 && exp10(-1.0) == 0.1);
	CHECK(fabs(exp10(0.5) - 3.1622776601683795) < 1e-15);
	CHECK(exp10f(2.0f) == 100.0f);
	CHECK(isinf(exp10(400.0)));

	CHECK(cbrt(-27.0) == -3.0 && cbrt(0.001) > 0.0999999 && cbrt(0.001) < 0.1000001);
	CHECK(cbrtf(8.0f) == 2.0f);
	CHECK(isnan(cbrt(NAN)) && isinf(cbrtf(-INFINITY)));

#if LDBL_MANT_DIG == 64 || LDBL_MANT_DIG == 53
	volatile long double base = 2.0L, half = 0.5L;
	CHECK(powl(base, 10.0L) == 1024.0L);
	CHECK(powl(9.0L, half) == 3.0L);
	CHECK(powl(-2.0L, 3.0L) == -8.0L);
	CHECK(isnan(powl(-1.0L, half)));
	CHECK(powl(0.0L, -1.0L) == INFINITY);
#endif
	return t_status;
}
