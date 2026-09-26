/*
 * The scanf family: integers in each base and size; floating-point numbers of
 * each size, and prefixes that are not whole numbers; strings, characters and
 * scan sets with widths; suppression, positional arguments, %n and literal
 * matching; allocation with m and wide strings; the count after a matching
 * failure and EOF after an input failure; and fscanf on a stream whose buffer
 * is smaller than its fields.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

#include "check.h"

int main(void)
{
	int i, j, k, n;
	unsigned u, v, w;
	signed char hh;
	short h;
	long l;
	long long ll;
	float f;
	double d, inf, nan;
	long double ld;
	char s[32], t[32], c[4], pair[4], *m, buffer[4];
	wchar_t ws[8];
	void *p;
	FILE *file;

	/* Integers. */
	CHECK(sscanf("42 -17 0x1F 017", "%d%i%i%i", &i, &j, &k, &n) == 4);
	CHECK(i == 42 && j == -17 && k == 31 && n == 15);
	CHECK(sscanf("ff 777 4294967295", "%x%o%u", &u, &v, &w) == 3);
	CHECK(u == 255 && v == 511 && w == UINT_MAX);
	CHECK(sscanf("-1 300 70000 9223372036854775807", "%hhd%hd%ld%lld", &hh, &h, &l, &ll) == 4);
	CHECK(hh == -1 && h == 300 && l == 70000 && ll == LLONG_MAX);
	CHECK(sscanf("0x10 +8", "%p%d", &p, &i) == 2 && p == (void *)16 && i == 8);
	CHECK(sscanf("12345", "%3d%d", &i, &j) == 2 && i == 123 && j == 45);
	CHECK(sscanf("08", "%i%d", &i, &j) == 2 && i == 0 && j == 8);
	CHECK(sscanf("0x", "%x", &u) == 0);
	CHECK(sscanf("-", "%d", &i) == 0);

	/* Floating-point numbers. */
	CHECK(sscanf("1.5 -2.25e2 0x1p-1", "%f%lf%Lf", &f, &d, &ld) == 3);
	CHECK(f == 1.5f && d == -225.0 && ld == 0.5L);
	CHECK(sscanf("inf NaN infinity", "%lf%lf%lf", &inf, &nan, &d) == 3);
	CHECK(inf > 1e308 && nan != nan && d == inf);
	CHECK(sscanf("1e+", "%f", &f) == 0);
	CHECK(sscanf("3.25kg", "%lf%s", &d, s) == 2 && d == 3.25 && !strcmp(s, "kg"));

	/* Strings, characters and scan sets. */
	CHECK(sscanf("  hello world", "%3s%s", s, t) == 2 && !strcmp(s, "hel") && !strcmp(t, "lo"));
	CHECK(sscanf("abcdef", "%3c", c) == 1 && !memcmp(c, "abc", 3));
	CHECK(sscanf("x y", "%c%c", pair, pair + 1) == 2 && pair[0] == 'x' && pair[1] == ' ');
	CHECK(sscanf("key=value;rest", "%[^=]=%[a-z]", s, t) == 2 && !strcmp(s, "key") && !strcmp(t, "value"));
	CHECK(sscanf("]-x", "%[]-]", s) == 1 && !strcmp(s, "]-"));
	CHECK(sscanf("cab-", "%[a-c]", s) == 1 && !strcmp(s, "cab"));
	CHECK(sscanf("abc", "%[0-9]", s) == 0);

	/* Suppression, positions, %n and literal matching. */
	CHECK(sscanf("10 20 30", "%*d %d%n", &i, &n) == 1 && i == 20 && n == 5);
	CHECK(sscanf("7 8", "%2$d %1$d", &i, &j) == 2 && i == 8 && j == 7);
	CHECK(sscanf("100 %", "%d %%", &i) == 1 && i == 100);
	CHECK(sscanf("x=5", "y=%d", &i) == 0);

	/* A matching failure counts what was stored; an input failure before the
	 * first field is EOF. */
	CHECK(sscanf("", "%d", &i) == EOF);
	CHECK(sscanf("   ", "%d", &i) == EOF);
	CHECK(sscanf("abc", "%d", &i) == 0);
	CHECK(sscanf("5", "%d %d", &i, &j) == 1);

	/* Allocation, and wide strings. */
	m = 0;
	CHECK(sscanf("allocated text", "%ms", &m) == 1 && m && !strcmp(m, "allocated"));
	free(m);
	CHECK(sscanf("wide", "%ls", ws) == 1 && ws[0] == L'w' && ws[3] == L'e' && ws[4] == 0);

	/* A stream with a four-byte buffer, so every field crosses a refill. */
	file = fopen("input", "w");
	CHECK(file && fputs("12345 67.5 name\n", file) >= 0 && fclose(file) == 0);
	file = fopen("input", "r");
	CHECK(file != 0);
	if (file) {
		CHECK(setvbuf(file, buffer, _IOFBF, sizeof buffer) == 0);
		CHECK(fscanf(file, "%d%lf%s", &i, &d, s) == 3);
		CHECK(i == 12345 && d == 67.5 && !strcmp(s, "name"));
		CHECK(fscanf(file, "%d", &i) == EOF);
		fclose(file);
	}
	return t_status;
}
