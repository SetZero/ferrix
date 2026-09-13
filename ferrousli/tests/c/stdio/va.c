/*
 * The library's variadic entry thunks and its va_list reader, from real C.
 *
 * snprintf is called with more integer arguments than the six integer
 * registers and more doubles than the eight vector registers, mixed with
 * long doubles, which always travel on the stack. A variadic function of the
 * program's own then reads two arguments itself and passes the rest on as a
 * va_list, and a copy made with va_copy, to vsnprintf and vfprintf.
 */

#define _GNU_SOURCE
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "test.h"

static void check(const char *what, const char *got, int n, const char *want)
{
	if (n != (int)strlen(want) || strcmp(got, want) != 0)
		t_error("%s: got [%s] (%d), want [%s]\n", what, got, n, want);
}

static const char REST[] = "7 8.5 x 9 1.5 2.5 3.5 4.5 5.5 6.5 7.5 8.5 9.5 10 11 12 0x2a";

static void rest(const char *fmt, ...)
{
	va_list ap, copy, again;
	char a[256], b[256];
	char *c;
	int n;

	va_start(ap, fmt);
	int first = va_arg(ap, int);
	double second = va_arg(ap, double);
	if (first != 42 || second != 0.5)
		t_error("the program's own va_arg read %d and %g\n", first, second);
	va_copy(copy, ap);
	va_copy(again, ap);

	n = vsnprintf(a, sizeof a, fmt, ap);
	check("vsnprintf of a consumed list", a, n, REST);
	n = vsnprintf(b, sizeof b, fmt, copy);
	check("vsnprintf of a copy", b, n, REST);
	n = vasprintf(&c, fmt, again);
	check("vasprintf of a copy", n < 0 ? "" : c, n, REST);
	if (n >= 0)
		free(c);

	va_end(again);
	va_end(copy);
	va_end(ap);
}

int main(void)
{
	char buf[512];
	int n;

	n = snprintf(buf, sizeof buf,
		"%d %lld %f %Lf %p %d %lld %f %Lf %s %d %g %d %g %g %g %g %g %g %g %c %e %La %d",
		1, 2LL, 3.5, 4.25L, (void *)0x10, 5, -6LL, 7.125, 8.5L, "nine", 10,
		11.0, 12, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 'x', 20.0, 1.0L, 21);
	check("snprintf of mixed arguments", buf, n,
		"1 2 3.500000 4.250000 0x10 5 -6 7.125000 8.500000 nine 10 11 12 "
		"13 14 15 16 17 18 19 x 2.000000e+01 0x8p-3 21");

	n = snprintf(buf, sizeof buf, "%2$Lg %1$d %4$g %3$s %6$g %5$d %8$g %7$d %9$g %10$g %11$g %12$g %13$g",
		1, 2.5L, "three", 4.0, 5, 6.0, 7, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0);
	check("snprintf of positional arguments", buf, n,
		"2.5 1 4 three 6 5 8 7 9 10 11 12 13");

	rest("%d %Lg %c %lld %g %g %g %g %g %g %g %g %g %d %d %d %p",
		42, 0.5, 7, 8.5L, 'x', 9LL, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5, 9.5,
		10, 11, 12, (void *)0x2a);
	return t_status;
}
