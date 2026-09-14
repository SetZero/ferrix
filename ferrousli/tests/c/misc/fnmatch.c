/*
 * fnmatch, adapted from libc-test's src/functional/fnmatch.c (MIT), which
 * took its cases from dietlibc and glibc, with more cases for
 * FNM_LEADING_DIR, classes, collating elements, high bytes and many stars.
 * Every expectation was checked against the host glibc 2.43, which agrees
 * with all the added cases and differs on five of libc-test's: it treats a
 * backslash inside a bracket as an escape ("[[?*\\]", "[]?*\\]"), matches the
 * malformed "[![:d-d]" and "[[:d-d]" brackets, and never matches a trailing
 * lone backslash ("\\" against "\\"). Those follow musl and POSIX here.
 *
 * An expectation of -FNM_NOMATCH marks a pattern POSIX leaves unspecified or
 * calls an error; any nonzero result is accepted for it.
 */

#define _GNU_SOURCE
#include <fnmatch.h>
#include <string.h>
#include <unistd.h>
#include "check.h"

static const struct {
	const char *pattern;
	const char *string;
	int flags;
	int expected;
} tests[] = {
	/* dietlibc */
	{ "*.c", "foo.c", 0, 0 },
	{ "*.c", ".c", 0, 0 },
	{ "*.a", "foo.c", 0, FNM_NOMATCH },
	{ "*.c", ".foo.c", 0, 0 },
	{ "*.c", ".foo.c", FNM_PERIOD, FNM_NOMATCH },
	{ "*.c", "foo.c", FNM_PERIOD, 0 },
	{ "a\\*.c", "a*.c", FNM_NOESCAPE, FNM_NOMATCH },
	{ "a\\*.c", "ax.c", 0, FNM_NOMATCH },
	{ "a[xy].c", "ax.c", 0, 0 },
	{ "a[!y].c", "ax.c", 0, 0 },
	{ "a[a/z]*.c", "a/x.c", FNM_PATHNAME, FNM_NOMATCH },
	{ "a/*.c", "a/x.c", FNM_PATHNAME, 0 },
	{ "a*.c", "a/x.c", FNM_PATHNAME, FNM_NOMATCH },
	{ "*/foo", "/foo", FNM_PATHNAME, 0 },
	{ "-O[01]", "-O1", 0, 0 },
	{ "[[?*\\]", "\\", 0, 0 },
	{ "[]?*\\]", "]", 0, 0 },
	/* initial right-bracket tests */
	{ "[!]a-]", "b", 0, 0 },
	{ "[]-_]", "^", 0, 0 },
	{ "[!]-_]", "X", 0, 0 },
	{ "??", "-", 0, FNM_NOMATCH },
	/* glibc */
	{ "*LIB*", "lib", FNM_PERIOD, FNM_NOMATCH },
	{ "a[/]b", "a/b", 0, 0 },
	{ "a[/]b", "a/b", FNM_PATHNAME, FNM_NOMATCH },
	{ "[a-z]/[a-z]", "a/b", 0, 0 },
	{ "*", "a/b", FNM_PATHNAME, FNM_NOMATCH },
	{ "*[/]b", "a/b", FNM_PATHNAME, FNM_NOMATCH },
	{ "*[b]", "a/b", FNM_PATHNAME, FNM_NOMATCH },
	{ "[*]/b", "a/b", 0, FNM_NOMATCH },
	{ "[*]/b", "*/b", 0, 0 },
	{ "[?]/b", "a/b", 0, FNM_NOMATCH },
	{ "[?]/b", "?/b", 0, 0 },
	{ "[[a]/b", "a/b", 0, 0 },
	{ "[[a]/b", "[/b", 0, 0 },
	{ "\\*/b", "a/b", 0, FNM_NOMATCH },
	{ "\\*/b", "*/b", 0, 0 },
	{ "\\?/b", "a/b", 0, FNM_NOMATCH },
	{ "\\?/b", "?/b", 0, 0 },
	{ "[/b", "[/b", 0, 0 },
	{ "\\[/b", "[/b", 0, 0 },
	{ "??" "/b", "aa/b", 0, 0 },
	{ "???b", "aa/b", 0, 0 },
	{ "???b", "aa/b", FNM_PATHNAME, FNM_NOMATCH },
	{ "?a/b", ".a/b", FNM_PATHNAME | FNM_PERIOD, FNM_NOMATCH },
	{ "a/?b", "a/.b", FNM_PATHNAME | FNM_PERIOD, FNM_NOMATCH },
	{ "*a/b", ".a/b", FNM_PATHNAME | FNM_PERIOD, FNM_NOMATCH },
	{ "a/*b", "a/.b", FNM_PATHNAME | FNM_PERIOD, FNM_NOMATCH },
	{ "[.]a/b", ".a/b", FNM_PATHNAME | FNM_PERIOD, FNM_NOMATCH },
	{ "a/[.]b", "a/.b", FNM_PATHNAME | FNM_PERIOD, FNM_NOMATCH },
	{ "*/?", "a/b", FNM_PATHNAME | FNM_PERIOD, 0 },
	{ "?/*", "a/b", FNM_PATHNAME | FNM_PERIOD, 0 },
	{ ".*/?", ".a/b", FNM_PATHNAME | FNM_PERIOD, 0 },
	{ "*/.?", "a/.b", FNM_PATHNAME | FNM_PERIOD, 0 },
	{ "*/*", "a/.b", FNM_PATHNAME | FNM_PERIOD, FNM_NOMATCH },
	{ "*?*/*", "a/.b", FNM_PERIOD, 0 },
	{ "*[.]/b", "a./b", FNM_PATHNAME | FNM_PERIOD, 0 },
	{ "*[[:alpha:]]/*[[:alnum:]]", "a/b", FNM_PATHNAME, 0 },
	{ "*[![:digit:]]*/[![:d-d]", "a/b", FNM_PATHNAME, -FNM_NOMATCH },
	{ "*[![:digit:]]*/[[:d-d]", "a/[", FNM_PATHNAME, -FNM_NOMATCH },
	{ "*[![:digit:]]*/[![:d-d]", "a/[", FNM_PATHNAME, -FNM_NOMATCH },
	{ "a?b", "a.b", FNM_PATHNAME | FNM_PERIOD, 0 },
	{ "a*b", "a.b", FNM_PATHNAME | FNM_PERIOD, 0 },
	{ "a[.]b", "a.b", FNM_PATHNAME | FNM_PERIOD, 0 },
	{ "\\", "\\", 0, 0 },
	{ "\\", "", 0, FNM_NOMATCH },
	{ "/", "\0", FNM_PATHNAME, FNM_NOMATCH },
	{ "\\/", "/", FNM_PATHNAME, 0 },
	{ "a", "A", FNM_CASEFOLD, 0 },
	{ "aaAA", "AaAa", FNM_CASEFOLD, 0 },
	{ "[a]", "A", FNM_CASEFOLD, 0 },
	{ "[!a]", "A", FNM_CASEFOLD, FNM_NOMATCH },
	{ "[!A-C]", "b", FNM_CASEFOLD, FNM_NOMATCH },
	{ "[!a-c]", "B", FNM_CASEFOLD, FNM_NOMATCH },
	{ "[!a-c]", "d", FNM_CASEFOLD, 0 },

	/* Added: a leading directory. */
	{ "a/b", "a/b/c", FNM_PATHNAME | FNM_LEADING_DIR, 0 },
	{ "a/b", "a/b/c", FNM_PATHNAME, FNM_NOMATCH },
	{ "a/b", "a/bc", FNM_LEADING_DIR, FNM_NOMATCH },
	{ "a*", "abc/def", FNM_PATHNAME | FNM_LEADING_DIR, 0 },
	{ "x", "x/y/z", FNM_LEADING_DIR, 0 },
	/* Added: classes, with folding, and unknown ones. */
	{ "[[:upper:]]", "A", 0, 0 },
	{ "[[:upper:]]", "a", 0, FNM_NOMATCH },
	{ "[[:space:]x]", " ", 0, 0 },
	{ "[![:alnum:]]", "_", 0, 0 },
	{ "[[:xdigit:]][[:punct:]]", "f!", 0, 0 },
	{ "[[:bogus:]]", "a", 0, -FNM_NOMATCH },
	/* Added: collating symbols and equivalence classes of one byte. */
	{ "[[.a.]]b", "ab", 0, 0 },
	{ "[[=a=]]b", "ab", 0, 0 },
	{ "[[.-.]]", "-", 0, 0 },
	{ "[![.a.]]", "a", 0, FNM_NOMATCH },
	/* Added: high bytes are ordinary characters in the C locale. */
	{ "\x80[\x81-\xff]?", "\x80\xc0\xff", 0, 0 },
	{ "[!\x80]", "\x80", 0, FNM_NOMATCH },
	/* Added: many stars, and escapes. */
	{ "*a*b*c", "xaybzc", 0, 0 },
	{ "*a*b*c", "xaybz", 0, FNM_NOMATCH },
	{ "*a*a*a*a*a*a*a*a*a*b", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 0, FNM_NOMATCH },
	{ "\\.x", ".x", FNM_PERIOD, 0 },
	{ "?x", ".x", FNM_PERIOD, FNM_NOMATCH },
	{ "a\\", "a\\", FNM_NOESCAPE, 0 },
	{ "[a-]", "-", 0, 0 },
	{ "", "", 0, 0 },
	{ "", "a", 0, FNM_NOMATCH },
	{ "*", "", 0, 0 },
};

int main(void)
{
	for (size_t i = 0; i < sizeof tests / sizeof *tests; i++) {
		int r = fnmatch(tests[i].pattern, tests[i].string, tests[i].flags);
		int x = tests[i].expected;
		if (r != x && (x != -FNM_NOMATCH || r == 0)) {
			write(2, "fnmatch(\"", 9);
			write(2, tests[i].pattern, strlen(tests[i].pattern));
			write(2, "\", \"", 4);
			write(2, tests[i].string, strlen(tests[i].string));
			write(2, "\") gave the wrong result\n", 25);
			t_status = 1;
		}
	}
	return t_status;
}
