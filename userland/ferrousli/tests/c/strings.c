/*
 * The string and memory functions, called through pointers the compiler cannot
 * see through, so it cannot fold the calls into constants. Exits zero, or with
 * the number of the check that failed.
 */

#include <string.h>

static size_t (*volatile strlen_p)(const char *) = strlen;
static int (*volatile strcmp_p)(const char *, const char *) = strcmp;
static int (*volatile strncmp_p)(const char *, const char *, size_t) = strncmp;
static void *(*volatile memcpy_p)(void *, const void *, size_t) = memcpy;
static void *(*volatile memmove_p)(void *, const void *, size_t) = memmove;
static void *(*volatile memset_p)(void *, int, size_t) = memset;
static int (*volatile memcmp_p)(const void *, const void *, size_t) = memcmp;

struct big {
	char bytes[512];
};

/* At -O2 this assignment becomes a call to memcpy the source never makes. */
__attribute__((noinline))
static void copy(struct big *to, const struct big *from)
{
	*to = *from;
}

int main(void)
{
	char buf[16];

	if (strlen_p("") != 0)
		return 1;
	if (strlen_p("ferrousli") != 9)
		return 2;

	memset_p(buf, 'x', 8);
	buf[8] = 0;
	if (strcmp_p(buf, "xxxxxxxx") != 0)
		return 3;

	memcpy_p(buf, "abcdefgh", 9);
	memmove_p(buf + 2, buf, 6);
	if (memcmp_p(buf, "ababcdef", 9) != 0)
		return 4;
	memmove_p(buf, buf + 2, 6);
	if (memcmp_p(buf, "abcdefef", 9) != 0)
		return 5;

	if (memcmp_p("a", "b", 1) >= 0)
		return 6;
	if (memcmp_p("\xff", "a", 1) <= 0)
		return 7;
	if (strcmp_p("abc", "abd") >= 0)
		return 8;
	if (strncmp_p("abcx", "abcy", 3) != 0)
		return 9;
	if (strcmp_p("\xff", "a") <= 0)
		return 10;

	static struct big from, to;
	for (int i = 0; i < 512; i++)
		from.bytes[i] = (char)i;
	copy(&to, &from);
	if (memcmp_p(&to, &from, sizeof to) != 0)
		return 11;

	return 0;
}
