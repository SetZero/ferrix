/*
 * strtod_l, strtof_l and strtold_l, which libc++'s number parsing calls.
 * Every locale here has '.' as its radix, so each parses as its plain form.
 */
#define _GNU_SOURCE
#include <locale.h>
#include <stdlib.h>

#include "check.h"

int main(void)
{
	locale_t c = newlocale(LC_ALL_MASK, "C", (locale_t)0);
	CHECK(c != (locale_t)0);
	const char *text = "1.5e3xyz";
	char *end = 0;
	CHECK(strtod_l(text, &end, c) == 1500.0 && end == text + 5);
	end = 0;
	CHECK(strtof_l(text, &end, c) == 1500.0f && end == text + 5);
	end = 0;
	long double ld = strtold_l("0.1", &end, c);
	CHECK(ld == 0.1L && *end == 0);
	CHECK(ld != 0.1);
	freelocale(c);
	return t_status;
}
