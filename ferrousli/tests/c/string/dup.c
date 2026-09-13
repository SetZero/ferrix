/* strdup and strndup: fresh copies that can be freed, and strndup's bound. */

#define _GNU_SOURCE
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

int main(void)
{
	const char *source = "ferrousli";

	char *copy = strdup(source);
	CHECK(copy != 0 && copy != source);
	CHECK(strcmp(copy, source) == 0);
	copy[0] = 'F';
	CHECK(source[0] == 'f');
	free(copy);

	char *empty = strdup("");
	CHECK(empty != 0 && empty[0] == 0);
	free(empty);

	/* strndup stops at n bytes, or at the NUL, whichever comes first. */
	char *prefix = strndup(source, 4);
	CHECK(prefix != 0 && strcmp(prefix, "ferr") == 0);
	free(prefix);
	char *whole = strndup(source, 100);
	CHECK(whole != 0 && strcmp(whole, source) == 0);
	free(whole);
	char *none = strndup(source, 0);
	CHECK(none != 0 && none[0] == 0);
	free(none);

	/* strndup never reads past n bytes: this array has no NUL. */
	static const char unterminated[3] = { 'a', 'b', 'c' };
	char *bounded = strndup(unterminated, 3);
	CHECK(bounded != 0 && strcmp(bounded, "abc") == 0);
	free(bounded);

	/* A copy of a long string. */
	static char big[100000];
	memset(big, 'x', sizeof big - 1);
	char *long_copy = strdup(big);
	CHECK(long_copy != 0 && strlen(long_copy) == sizeof big - 1);
	CHECK(memcmp(long_copy, big, sizeof big) == 0);
	free(long_copy);

	return t_status;
}
