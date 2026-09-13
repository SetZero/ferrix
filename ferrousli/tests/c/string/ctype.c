/*
 * The ctype.h functions in the C locale, called as functions rather than
 * through musl's macros, and glibc's tables, which programs built against
 * glibc's headers read instead.
 */

#define _GNU_SOURCE
#include <ctype.h>
#include "check.h"

/* glibc's accessors, which musl's headers do not declare. */
const unsigned short **__ctype_b_loc(void);
const int **__ctype_tolower_loc(void);
const int **__ctype_toupper_loc(void);

/* glibc's class bits on a little-endian machine. */
enum {
	IS_UPPER = 0x100, IS_LOWER = 0x200, IS_ALPHA = 0x400, IS_DIGIT = 0x800,
	IS_XDIGIT = 0x1000, IS_SPACE = 0x2000, IS_PRINT = 0x4000,
	IS_GRAPH = 0x8000, IS_BLANK = 0x1, IS_CNTRL = 0x2, IS_PUNCT = 0x4,
	IS_ALNUM = 0x8,
};

static int (*const volatile classify[])(int) = {
	isalnum, isalpha, isblank, iscntrl, isdigit, isgraph, islower, isprint,
	ispunct, isspace, isupper, isxdigit, isascii,
};

static const int bits[] = {
	IS_ALNUM, IS_ALPHA, IS_BLANK, IS_CNTRL, IS_DIGIT, IS_GRAPH, IS_LOWER,
	IS_PRINT, IS_PUNCT, IS_SPACE, IS_UPPER, IS_XDIGIT, 0,
};

/* What each class should say, by the C standard's definitions. */
static int expected(int which, int c)
{
	int upper = c >= 'A' && c <= 'Z';
	int lower = c >= 'a' && c <= 'z';
	int digit = c >= '0' && c <= '9';
	int graph = c > 0x20 && c < 0x7f;
	switch (which) {
	case 0: return upper || lower || digit;
	case 1: return upper || lower;
	case 2: return c == ' ' || c == '\t';
	case 3: return (c >= 0 && c < 0x20) || c == 0x7f;
	case 4: return digit;
	case 5: return graph;
	case 6: return lower;
	case 7: return c >= 0x20 && c < 0x7f;
	case 8: return graph && !(upper || lower || digit);
	case 9: return c == ' ' || (c >= '\t' && c <= '\r');
	case 10: return upper;
	case 11: return digit || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
	default: return c >= 0 && c < 128;
	}
}

int main(void)
{
	const unsigned short *table = *__ctype_b_loc();
	const int *lower = *__ctype_tolower_loc();
	const int *upper = *__ctype_toupper_loc();

	for (int which = 0; which < 13; which++)
		for (int c = -1; c < 256; c++) {
			int want = expected(which, c);
			if (!!classify[which](c) != want) {
				CHECK(!!classify[which](c) == want);
				return t_status;
			}
			if (bits[which] && !!(table[c] & bits[which]) != want) {
				CHECK(!!(table[c] & bits[which]) == want);
				return t_status;
			}
		}

	for (int c = -128; c < 256; c++) {
		int want_lower = c >= 'A' && c <= 'Z' ? c + 32 : c == -1 ? -1 : c < 0 ? c + 256 : c;
		int want_upper = c >= 'a' && c <= 'z' ? c - 32 : c == -1 ? -1 : c < 0 ? c + 256 : c;
		CHECK(lower[c] == want_lower);
		CHECK(upper[c] == want_upper);
		if (c < 0)
			CHECK(table[c] == 0);
		if (c >= -1) {
			CHECK((tolower)(c) == (c >= 'A' && c <= 'Z' ? c + 32 : c));
			CHECK((toupper)(c) == (c >= 'a' && c <= 'z' ? c - 32 : c));
		}
		if (t_status)
			return t_status;
	}

	CHECK((toascii)(0x1c1) == 0x41);
	CHECK((isascii)(-1) == 0);
	CHECK((isalpha)(0x141) == 0);

	return t_status;
}
