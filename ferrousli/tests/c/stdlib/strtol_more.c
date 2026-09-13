/*
 * The integer parsers beyond libc-test's cases: C23's 0b prefix under glibc's
 * __isoc23_ names, the inttypes.h functions, atoi and friends, and the
 * prefix and error edge cases, all called through pointers the compiler cannot
 * fold.
 */

#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <stdlib.h>
#include "check.h"

/* glibc 2.38's headers redirect strtol and the rest to these under C23. */
long __isoc23_strtol(const char *, char **, int);
unsigned long __isoc23_strtoul(const char *, char **, int);
long long __isoc23_strtoll(const char *, char **, int);
unsigned long long __isoc23_strtoull(const char *, char **, int);
intmax_t __isoc23_strtoimax(const char *, char **, int);
uintmax_t __isoc23_strtoumax(const char *, char **, int);

static long (*volatile strtol_p)(const char *, char **, int) = strtol;
static long (*volatile c23_strtol_p)(const char *, char **, int) = __isoc23_strtol;

/* Parses s with f in base, and checks value, consumed length and errno. */
#define EXPECT(f, s, base, value, consumed, err) do { \
	const char *str_ = (s); \
	char *end_ = 0; \
	errno = 0; \
	CHECK((f)(str_, &end_, (base)) == (value)); \
	CHECK(end_ - str_ == (consumed)); \
	CHECK(errno == (err)); \
} while (0)

int main(void)
{
	EXPECT(strtol_p, "0b101", 0, 0, 1, 0);
	EXPECT(strtol_p, "0b101", 2, 0, 1, 0);
	EXPECT(c23_strtol_p, "0b101", 0, 5, 5, 0);
	EXPECT(c23_strtol_p, "-0B11", 2, -3, 5, 0);
	EXPECT(c23_strtol_p, "0b", 0, 0, 1, 0);
	EXPECT(c23_strtol_p, "0b2", 2, 0, 1, 0);
	EXPECT(c23_strtol_p, "0b101", 16, 0xb101, 5, 0);
	EXPECT(c23_strtol_p, "0b101", 10, 0, 1, 0);
	EXPECT(c23_strtol_p, "0x1f", 0, 31, 4, 0);
	EXPECT(__isoc23_strtoul, "0b11111111", 0, 255UL, 10, 0);
	EXPECT(__isoc23_strtoll, "-0b1", 0, -1LL, 4, 0);
	EXPECT(__isoc23_strtoull, "0B1", 2, 1ULL, 3, 0);
	EXPECT(__isoc23_strtoimax, "0b1000", 0, 8, 6, 0);
	EXPECT(__isoc23_strtoumax, "-0b1", 0, UINTMAX_MAX, 4, 0);

	EXPECT(strtol_p, "0x", 0, 0, 1, 0);
	EXPECT(strtol_p, "-0xg", 16, 0, 2, 0);
	EXPECT(strtol_p, "08", 0, 0, 1, 0);
	EXPECT(strtol_p, "0755", 0, 0755, 4, 0);
	EXPECT(strtol_p, "\t\n\v\f\r +42", 10, 42, 9, 0);
	EXPECT(strtol_p, "", 10, 0, 0, EINVAL);
	EXPECT(strtol_p, " -", 10, 0, 0, EINVAL);
	EXPECT(strtol_p, "12", 1, 0, 0, EINVAL);
	EXPECT(strtol_p, "12", -3, 0, 0, EINVAL);
	EXPECT(strtol_p, "zz", 36, 35 * 36 + 35, 2, 0);
	EXPECT(strtol_p, "99999999999999999999999x", 10, LONG_MAX, 23, ERANGE);

	EXPECT(strtoimax, "-9223372036854775808", 10, INTMAX_MIN, 20, 0);
	EXPECT(strtoimax, "-9223372036854775809", 10, INTMAX_MIN, 20, ERANGE);
	EXPECT(strtoumax, "0xffffffffffffffff", 0, UINTMAX_MAX, 18, 0);
	EXPECT(strtoumax, "0x10000000000000000", 0, UINTMAX_MAX, 19, ERANGE);

	/* A null endptr is allowed. */
	CHECK(strtol_p("77", 0, 8) == 077);

	/* atoi and friends: decimal only, no errno. */
	errno = 0;
	CHECK(atoi("  -123abc") == -123);
	CHECK(atoi("0x10") == 0);
	CHECK(atol("+9223372036854775807") == LONG_MAX);
	CHECK(atoll("-9223372036854775808") == LLONG_MIN);
	CHECK(atoi("") == 0);
	CHECK(errno == 0);
	return t_status;
}
