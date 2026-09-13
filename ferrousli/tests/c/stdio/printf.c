/*
 * The printf family beyond libc-test's snprintf test: every length, flag and
 * conversion, positions and stars, the glibc spellings of null pointers,
 * exact floating point for double and long double including subnormals and
 * precisions past 1000, hexadecimal output, errors and errno, and each
 * destination: strings, allocated strings, descriptors and streams.
 *
 * The expected floating-point strings are the host glibc's output.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <float.h>
#include <limits.h>
#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <wchar.h>
#include "test.h"

#define T(want, ...) do { \
	char b_[2048]; \
	int n_ = snprintf(b_, sizeof b_, __VA_ARGS__); \
	if (n_ != (int)strlen(want) || strcmp(b_, want) != 0) \
		t_error("snprintf(%s) = %d [%s], want [%s]\n", #__VA_ARGS__, n_, b_, want); \
} while (0)

#define FAILS(error, ...) do { \
	char b_[64]; \
	errno = 0; \
	int n_ = snprintf(b_, sizeof b_, __VA_ARGS__); \
	if (n_ != -1 || errno != (error)) \
		t_error("snprintf(%s) = %d, errno %d, want -1 and %d\n", #__VA_ARGS__, n_, errno, error); \
} while (0)

#define CHECK(c) do { \
	if (!(c)) \
		t_error("%s failed (errno %d)\n", #c, errno); \
} while (0)

static void integers(void)
{
	T("+005", "%+.3d", 5);
	T("ff", "% x", 255);
	T("0XFF", "%#X", 255);
	T("  010", "%#5o", 8);
	T("0xff    |", "%-#8x|", 255);
	T("     007", "%08.3d", 7);
	T("-1", "%hhd", 255);
	T("44", "%hhd", 300);
	T("255", "%hhu", -1);
	T("65535", "%hu", -1);
	T("-32768", "%hd", 32768);
	T("-2147483648", "%d", INT_MIN);
	T("4294967295", "%u", UINT_MAX);
	T("-9223372036854775808", "%ld", LONG_MIN);
	T("18446744073709551615", "%llu", -1LL);
	T("-9223372036854775808", "%jd", INTMAX_MIN);
	T("-1", "%zd", (ssize_t)-1);
	T("18446744073709551615", "%zu", SIZE_MAX);
	T("ffffffffffffffff", "%tx", (ptrdiff_t)-1);
	T("1777777777777777777777", "%llo", -1LL);
	T("1234567890123", "%Ld", 1234567890123LL);
	T("5", "%qd", 5LL);
	T("1234567", "%'d", 1234567);
	T("100%", "%d%%", 100);
	T("%", "%5%");
	T("   42", "%*d", 5, 42);
	T("42   |", "%*d|", -5, 42);
	T("42", "%.*d", -1, 42);
	T("00042", "%.*d", 5, 42);
	T("-0042", "%05d", -42);
	T("  -42", "%5d", -42);
}

static void pointers_strings_and_characters(void)
{
	T("(nil)", "%p", (void *)0);
	T("0x1234", "%p", (void *)0x1234);
	T("     (nil)", "%10p", (void *)0);
	T("0x000001", "%08p", (void *)1);
	T("+0x1", "%+p", (void *)1);
	T("(null)", "%s", (char *)0);
	T("", "%.3s", (char *)0);
	T("(null)", "%.6s", (char *)0);
	T("he", "%.2s", "hello");
	T("   hi", "%5s", "hi");
	T("hi   |", "%-5s|", "hi");
	T("   hi", "%05s", "hi");
	T("    x", "%5c", 'x');
	T("x    |", "%-5c|", 'x');
	T("A", "%lc", (wint_t)'A');
	T("wide", "%ls", L"wide");
	T("wi", "%.2ls", L"wide");
	FAILS(EILSEQ, "%ls", L"\x263a");
	FAILS(EILSEQ, "%lc", (wint_t)0x263a);
	errno = ENOENT;
	T("No such file or directory", "%m");
	errno = ENOENT;
	T("[No such]", "[%.7m]");
}

static void positions_and_counts(void)
{
	int n1 = 0;
	long long n2 = 0;
	signed char n3 = 0;
	short n4 = 0;
	size_t n5 = 0;

	T("b a b", "%2$s %1$s %2$s", "a", "b");
	T("   42", "%1$*2$d", 42, 5);
	T("3.14|7", "%2$.*1$f|%3$d", 2, 3.14159, 7);
	T("1.5 x", "%2$.1Lf %1$c", 'x', 1.5L);
	T("2", "%2$d", 1, 2);
	FAILS(EINVAL, "%1$d %d", 1, 2);
	FAILS(EINVAL, "%d %1$d", 1, 2);
	FAILS(EINVAL, "%0$d", 1);
	FAILS(EINVAL, "%65$d", 1);
	FAILS(EINVAL, "%y");
	FAILS(EINVAL, "abc%");
	FAILS(EINVAL, "%-");

	T("abc", "ab%nc%lln", &n1, &n2);
	CHECK(n1 == 2 && n2 == 3);
	T("xyz", "x%hhny%hnz%zn", &n3, &n4, &n5);
	CHECK(n3 == 1 && n4 == 2 && n5 == 3);
	T("", "%n", (int *)0);
}

static void overflow(void)
{
	char b[8];

	errno = 0;
	CHECK(snprintf(NULL, 0, "%*d", INT_MAX, 1) == INT_MAX);
	errno = 0;
	CHECK(snprintf(NULL, 0, "%*d%d", INT_MAX, 1, 2) == -1 && errno == EOVERFLOW);
	errno = 0;
	CHECK(snprintf(b, sizeof b, "%2147483648d", 1) == -1 && errno == EOVERFLOW);
	errno = 0;
	CHECK(snprintf(b, sizeof b, "%.*f", INT_MAX, 1.0) == -1 && errno == EOVERFLOW);
	CHECK(snprintf(b, sizeof b, "%s", "truncated") == 9 && strcmp(b, "truncat") == 0);
	CHECK(snprintf(b, 1, "%d", 5) == 1 && b[0] == 0);
}

static void doubles(void)
{
	char b[2048];
	int n;

	T("0x1p+0", "%a", 1.0);
	T("0x0.0000000000001p-1022", "%a", DBL_TRUE_MIN);
	T("0x1.fffffffffffffp+1023", "%a", DBL_MAX);
	T("0x2p+0", "%.0a", 0x1.fp0);
	T("0x2.0p+0", "%.1a", 0x1.f8p0);
	T("0x2p+0", "%.0a", 0x1.8p0);
	T("0x1p+0", "%.0a", 0x1.18p0);
	T("0x1p+0", "%.0a", 0x1.08p0);
	T("0x0.00p-1022", "%.2a", DBL_TRUE_MIN);
	T("0x1.p+0", "%#a", 1.0);
	T("0x0.000p+0", "%.3a", 0.0);
	T("-0x1.80p+0", "%10.2a", -1.5);
	T("0X1.FFP+7", "%A", 255.5);
	T("0x0p-1022", "%.0a", 0x0.8p-1022);
	T("0x1.555p-2", "%.3a", 1.0 / 3);
	T("0x1.55555555555550p-2", "%.14a", 1.0 / 3);
	T("0x00001p+0", "%010a", 1.0);

	T("-nan", "%f", -NAN);
	T("-inf", "%e", -INFINITY);
	T("NAN", "%F", NAN);
	T("  inf", "%5.1f", INFINITY);
	T("nan   ", "%-6f", NAN);
	T("  -inf", "%06f", -INFINITY);
	T("+inf", "%+g", INFINITY);
	T(" nan", "% a", NAN);
	T("INF", "%G", INFINITY);

	T("1.e+00", "%#.0e", 1.0);
	T("1.", "%#.0f", 1.0);
	T("1.00000", "%#g", 1.0);
	T("100000", "%g", 100000.0);
	T("0.5", "%.0g", 0.5);
	T("2.", "%#.0g", 2.5);
	T("-0.000000", "%f", -0.0);
	T("0", "%.0f", 0.5);
	T("2", "%.0f", 1.5);
	T("2", "%.0f", 2.5);
	T("0.12", "%.2f", 0.125);
	T("0.38", "%.2f", 0.375);
	T("1e+100", "%g", 1e100);
	T("1e+06", "%g", 999999.5);
	/* C keeps the zeros; glibc 2.43 prints "1.e+06" here. */
	T("1.00000e+06", "%#g", 999999.5);
	T("10.0", "%#.3g", 9.9996);
	T("0001.50", "%07.2f", 1.5);
	T("+1.50", "%+.2f", 1.5);
	T(" 1.50", "% .2f", 1.5);
	T("-001.5", "%06.1f", -1.5);
	T("1.500000E+00", "%E", 1.5);
	T("1.5E-10", "%G", 1.5e-10);
	T("2.22507e-308", "%g", DBL_MIN);
	T("4.94066e-324", "%g", DBL_TRUE_MIN);
	T("1.797693134862315708e+308", "%.18e", DBL_MAX);

	n = snprintf(b, sizeof b, "%.0f", DBL_MAX);
	CHECK(n == 309 && strncmp(b, "17976931348623157", 17) == 0);
	n = snprintf(b, sizeof b, "%.1074f", DBL_TRUE_MIN);
	CHECK(n == 1076 && strncmp(b, "0.000", 5) == 0);
	CHECK(strncmp(b + 2 + 323, "49406564584124654", 17) == 0);
	CHECK(strcmp(b + n - 3, "625") == 0);
	n = snprintf(b, sizeof b, "%.1100e", 1.0);
	CHECK(n == 1106 && strncmp(b, "1.000", 5) == 0 && strcmp(b + n - 4, "e+00") == 0);
	CHECK(snprintf(NULL, 0, "%.1500f", 1.0) == 1502);
	CHECK(snprintf(NULL, 0, "%.1500g", 0.1) == 57);
}

static void long_doubles(void)
{
	char b[8000];
	int n;

	T("0x8p-3", "%La", 1.0L);
	T("0xf.fffffffffffffffp+16380", "%La", LDBL_MAX);
	T("0x0.000000000000001p-16385", "%La", LDBL_TRUE_MIN);
	T("0x8p-16385", "%La", 0x1p-16382L);
	T("0x1p+4", "%.0La", 0xf.8p0L);
	T("0xf.fp+0", "%.1La", 0xf.fp0L);
	T("0x8p+0", "%.0La", 0x8.8p0L);
	T("0x9.1a2b3c4d5e6f78p-3", "%La", 0x1.23456789abcdefp0L);
	T("-0x0.000000000000003p-16385", "%La", -0x3p-16445L);
	T("0x0p+0", "%La", 0.0L);
	T("+inf", "%+Lf", (long double)INFINITY);
	T("-nan", "%Lg", (long double)-NAN);
	T("3.14159265358979324", "%.18Lg", 3.14159265358979323846264338327950288L);
	T("3.6452e-4951", "%.4Le", LDBL_TRUE_MIN);
	T("1.18973e+4932", "%Lg", LDBL_MAX);
	T("0.1", "%.1Lf", 0.05L);
	T("0.0", "%.1Lf", 0.0499999999999999999L);

	n = snprintf(b, sizeof b, "%.0Lf", LDBL_MAX);
	CHECK(n == 4933 && strncmp(b, "11897314953572317650", 20) == 0);
	n = snprintf(NULL, 0, "%.16445Lf", LDBL_TRUE_MIN);
	CHECK(n == 16447);
}

static void destinations(void)
{
	char b[64];
	char *s = NULL;
	int fds[2];
	FILE *f;

	CHECK(sprintf(b, "%05.1f|%s", 2.25, "x") == 7 && strcmp(b, "002.2|x") == 0);
	CHECK(asprintf(&s, "%s-%d", "x", 5) == 3 && s && strcmp(s, "x-5") == 0);
	free(s);
	s = (char *)1;
	CHECK(asprintf(&s, "%y") == -1 && s == (char *)1);

	CHECK(pipe(fds) == 0);
	CHECK(dprintf(fds[1], "fd %d\n", 3) == 5);
	CHECK(read(fds[0], b, sizeof b) == 5 && memcmp(b, "fd 3\n", 5) == 0);
	close(fds[0]);
	close(fds[1]);

	CHECK((f = fopen("out", "w+")) != NULL);
	CHECK(fprintf(f, "%3000d|%s", 7, "end") == 3004);
	CHECK(ftell(f) == 3004);
	rewind(f);
	CHECK(fseek(f, 2998, SEEK_SET) == 0 && fgets(b, sizeof b, f) && strcmp(b, " 7|end") == 0);
	CHECK(fclose(f) == 0);

	CHECK((f = fopen("out", "r")) != NULL);
	errno = 0;
	CHECK(fprintf(f, "x") == -1 && errno == EBADF && ferror(f));
	CHECK(fclose(f) == 0);
}

int main(void)
{
	integers();
	pointers_strings_and_characters();
	positions_and_counts();
	overflow();
	doubles();
	long_doubles();
	destinations();
	printf("printf %s\n", "done");
	return t_status;
}
