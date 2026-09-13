/*
 * Adapted from libc-test's src/functional/strtol.c (MIT). Its t_error
 * reports became CHECKs.
 */

#include <errno.h>
#include <stdlib.h>
#include "check.h"

/* r = place to store result, f = call to test, x = expected result */
#define TEST(r, f, x) (errno = 0, (r) = (f), CHECK((r) == (x)))
#define TEST2(r, f, x) ((r) = (f), CHECK((r) == (x)))

int main(void)
{
	int i;
	long l;
	unsigned long ul;
	long long ll;
	unsigned long long ull;
	char *s, *c;

	TEST(l, atol("2147483647"), 2147483647L);
	TEST(l, strtol("2147483647", 0, 0), 2147483647L);
	TEST(ul, strtoul("4294967295", 0, 0), 4294967295UL);

	CHECK(sizeof(long) == 8);
	TEST(l, strtol(s="9223372036854775808", &c, 0), 9223372036854775807L);
	TEST2(i, c-s, 19);
	TEST2(i, errno, ERANGE);
	TEST(l, strtol(s="-9223372036854775809", &c, 0), -9223372036854775807L-1);
	TEST2(i, c-s, 20);
	TEST2(i, errno, ERANGE);
	TEST(ul, strtoul(s="18446744073709551616", &c, 0), 18446744073709551615UL);
	TEST2(i, c-s, 20);
	TEST2(i, errno, ERANGE);
	TEST(ul, strtoul(s="-1", &c, 0), -1UL);
	TEST2(i, c-s, 2);
	TEST2(i, errno, 0);
	TEST(ul, strtoul(s="-2", &c, 0), -2UL);
	TEST2(i, c-s, 2);
	TEST2(i, errno, 0);
	TEST(ul, strtoul(s="-9223372036854775808", &c, 0), -9223372036854775808UL);
	TEST2(i, c-s, 20);
	TEST2(i, errno, 0);
	TEST(ul, strtoul(s="-9223372036854775809", &c, 0), -9223372036854775809UL);
	TEST2(i, c-s, 20);
	TEST2(i, errno, 0);
	TEST(ul, strtoul(s="-18446744073709551616", &c, 0), 18446744073709551615UL);
	TEST2(i, c-s, 21);
	TEST2(i, errno, ERANGE);

	CHECK(sizeof(long long) == 8);
	TEST(ll, strtoll(s="9223372036854775808", &c, 0), 9223372036854775807LL);
	TEST2(i, c-s, 19);
	TEST2(i, errno, ERANGE);
	TEST(ll, strtoll(s="-9223372036854775809", &c, 0), -9223372036854775807LL-1);
	TEST2(i, c-s, 20);
	TEST2(i, errno, ERANGE);
	TEST(ull, strtoull(s="18446744073709551616", &c, 0), 18446744073709551615ULL);
	TEST2(i, c-s, 20);
	TEST2(i, errno, ERANGE);
	TEST(ull, strtoull(s="-1", &c, 0), -1ULL);
	TEST2(i, c-s, 2);
	TEST2(i, errno, 0);
	TEST(ull, strtoull(s="-2", &c, 0), -2ULL);
	TEST2(i, c-s, 2);
	TEST2(i, errno, 0);
	TEST(ull, strtoull(s="-9223372036854775808", &c, 0), -9223372036854775808ULL);
	TEST2(i, c-s, 20);
	TEST2(i, errno, 0);
	TEST(ull, strtoull(s="-9223372036854775809", &c, 0), -9223372036854775809ULL);
	TEST2(i, c-s, 20);
	TEST2(i, errno, 0);
	TEST(ull, strtoull(s="-18446744073709551616", &c, 0), 18446744073709551615ULL);
	TEST2(i, c-s, 21);
	TEST2(i, errno, ERANGE);

	TEST(l, strtol("z", 0, 36), 35);
	TEST(l, strtol("00010010001101000101011001111000", 0, 2), 0x12345678);
	TEST(l, strtol(s="0F5F", &c, 16), 0x0f5f);

	TEST(l, strtol(s="0xz", &c, 16), 0);
	TEST2(i, c-s, 1);

	TEST(l, strtol(s="0x1234", &c, 16), 0x1234);
	TEST2(i, c-s, 6);

	c = NULL;
	TEST(l, strtol(s="123", &c, 37), 0);
	TEST2(i, c-s, 0);
	TEST2(i, errno, EINVAL);

	TEST(l, strtol(s="  15437", &c, 8), 015437);
	TEST2(i, c-s, 7);

	TEST(l, strtol(s="  1", &c, 0), 1);
	TEST2(i, c-s, 3);
	return t_status;
}
