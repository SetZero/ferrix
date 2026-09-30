/*
 * Copying, concatenation, spans, tokens and the BSD bounded copies.
 *
 * Adapted from libc-test's src/functional/string.c (MIT), with its checks
 * rewritten to use check.h.
 */

#define _GNU_SOURCE
#include <string.h>
#include "check.h"

int main(void)
{
	char b[32];
	char *s;

	b[16] = 'a'; b[17] = 'b'; b[18] = 'c'; b[19] = 0;
	CHECK((s = strcpy(b, b + 16)) == b);
	CHECK(!strcmp(s, "abc"));
	CHECK((s = strcpy(b + 1, b + 16)) == b + 1);
	CHECK(!strcmp(s, "abc"));
	CHECK((s = strcpy(b + 2, b + 16)) == b + 2);
	CHECK(!strcmp(s, "abc"));
	CHECK((s = strcpy(b + 3, b + 16)) == b + 3);
	CHECK(!strcmp(s, "abc"));

	CHECK((s = strcpy(b + 1, b + 17)) == b + 1);
	CHECK(!strcmp(s, "bc"));
	CHECK((s = strcpy(b + 2, b + 18)) == b + 2);
	CHECK(!strcmp(s, "c"));
	CHECK((s = strcpy(b + 3, b + 19)) == b + 3);
	CHECK(!strcmp(s, ""));

	CHECK(memset(b, 'x', sizeof b) == b);
	CHECK(strncpy(b, "abc", sizeof b - 1) == b);
	/* strncpy pads the destination with NULs */
	CHECK(memcmp(b, "abc\0\0\0\0", 8) == 0);
	/* and stops at n */
	CHECK(b[sizeof b - 1] == 'x');

	b[3] = 'x'; b[4] = 0;
	strncpy(b, "abc", 3);
	CHECK(b[2] == 'c');
	/* strncpy does not terminate a string that fills n */
	CHECK(b[3] == 'x');

	CHECK(!strncmp("abcd", "abce", 3));
	CHECK(!!strncmp("abc", "abd", 3));

	strcpy(b, "abc");
	CHECK(strncat(b, "123456", 3) == b);
	CHECK(b[6] == 0);
	CHECK(!strcmp(b, "abc123"));

	strcpy(b, "aaababccdd0001122223");
	CHECK(strchr(b, 'b') == b + 3);
	CHECK(strrchr(b, 'b') == b + 5);
	CHECK(strspn(b, "abcd") == 10);
	CHECK(strcspn(b, "0123") == 10);
	CHECK(strpbrk(b, "0123") == b + 10);

	strcpy(b, "abc   123; xyz; foo");
	CHECK((s = strtok(b, " ")) == b);
	CHECK(!strcmp(s, "abc"));
	CHECK((s = strtok(NULL, ";")) == b + 4);
	CHECK(!strcmp(s, "  123"));
	CHECK((s = strtok(NULL, " ;")) == b + 11);
	CHECK(!strcmp(s, "xyz"));
	CHECK((s = strtok(NULL, " ;")) == b + 16);
	CHECK(!strcmp(s, "foo"));
	CHECK(strtok(NULL, " ;") == NULL);

	memset(b, 'x', sizeof b);
	CHECK(strlcpy(b, "abc", sizeof b - 1) == 3);
	/* terminates a short string */
	CHECK(b[3] == 0);
	/* and writes nothing more */
	CHECK(b[4] == 'x');

	memset(b, 'x', sizeof b);
	CHECK(strlcpy(b, "abc", 2) == 3);
	CHECK(b[0] == 'a');
	CHECK(b[1] == 0);

	memset(b, 'x', sizeof b);
	CHECK(strlcpy(b, "abc", 3) == 3);
	CHECK(b[2] == 0);

	CHECK(strlcpy(NULL, "abc", 0) == 3);

	memcpy(b, "abc\0\0\0x\0", 8);
	CHECK(strlcat(b, "123", sizeof b) == 6);
	CHECK(!strcmp(b, "abc123"));

	memcpy(b, "abc\0\0\0x\0", 8);
	CHECK(strlcat(b, "123", 6) == 6);
	CHECK(!strcmp(b, "abc12"));
	CHECK(b[6] == 'x');

	memcpy(b, "abc\0\0\0x\0", 8);
	CHECK(strlcat(b, "123", 4) == 6);
	CHECK(!strcmp(b, "abc"));

	memcpy(b, "abc\0\0\0x\0", 8);
	CHECK(strlcat(b, "123", 3) == 6);
	CHECK(!strcmp(b, "abc"));

	return t_status;
}
