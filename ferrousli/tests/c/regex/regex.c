/*
 * regex.h from C: compiling, matching, submatch offsets, the flags, the
 * error codes and regerror's messages.
 *
 * The REG_ICASE bracket case is libc-test's regex-bracket-icase (MIT), from
 * Austin Group bug 872; the REG_NOSUB case is its regexec-nosub; the \0 case
 * is its regex-backref-0; the ERE back-reference case is its
 * regex-ere-backref; and [^aa-z] is its regex-negated-range.
 */

#include <regex.h>
#include <string.h>
#include <stdlib.h>
#include "check.h"

/* Matches `pat` against `s` and checks the first `n` offset pairs, given as
   2n integers, with -1 for a group that must not match. */
static void match_is(int cflags, const char *pat, const char *s, int eflags,
                     int n, const long *want)
{
	regex_t re;
	regmatch_t m[10];
	CHECK(n <= 10);
	if (regcomp(&re, pat, cflags) != 0) {
		CHECK(!"the pattern compiles");
		return;
	}
	for (int i = 0; i < 10; i++)
		m[i].rm_so = m[i].rm_eo = -2;
	int r = regexec(&re, s, (size_t)n, m, eflags);
	if (r != 0) {
		CHECK(!"the string matches");
		regfree(&re);
		return;
	}
	for (int i = 0; i < n; i++) {
		CHECK(m[i].rm_so == (regoff_t)want[2 * i]);
		CHECK(m[i].rm_eo == (regoff_t)want[2 * i + 1]);
	}
	regfree(&re);
}

/* Checks that `pat` compiles and does not match `s`. */
static void no_match(int cflags, const char *pat, const char *s, int eflags)
{
	regex_t re;
	if (regcomp(&re, pat, cflags) != 0) {
		CHECK(!"the pattern compiles");
		return;
	}
	CHECK(regexec(&re, s, 0, 0, eflags) == REG_NOMATCH);
	regfree(&re);
}

/* Checks that `pat` fails to compile with `code`, and that regerror gives
   `message` for it. */
static void bad_pattern(int cflags, const char *pat, int code, const char *message)
{
	regex_t re;
	char buf[128];
	int r = regcomp(&re, pat, cflags);
	CHECK(r == code);
	size_t n = regerror(r, &re, buf, sizeof buf);
	CHECK(n == strlen(message) + 1);
	CHECK(strcmp(buf, message) == 0);
}

int main(void)
{
	/* An extended expression, its groups, and re_nsub. */
	regex_t re;
	CHECK(regcomp(&re, "(a|ab)(c|bcd)(d*)", REG_EXTENDED) == 0);
	CHECK(re.re_nsub == 3);
	regmatch_t m[4];
	CHECK(regexec(&re, "xabcd", 4, m, 0) == 0);
	/* POSIX: the whole match is the longest at the leftmost place, then
	   each subexpression from the left is as long as it can be. */
	CHECK(m[0].rm_so == 1 && m[0].rm_eo == 5);
	CHECK(m[1].rm_so == 1 && m[1].rm_eo == 3);
	CHECK(m[2].rm_so == 3 && m[2].rm_eo == 4);
	CHECK(m[3].rm_so == 4 && m[3].rm_eo == 5);
	regfree(&re);

	static const long whole_and_two[] = { 0, 3, 0, 2, 2, 3 };
	match_is(REG_EXTENDED, "(ab|a)(bc|c)", "abc", 0, 3, whole_and_two);

	/* A group that does not take part is reported as -1, -1. */
	static const long second_only[] = { 0, 2, -1, -1, 1, 2 };
	match_is(REG_EXTENDED, "x(a)?(b)?", "xb", 0, 3, second_only);

	/* A repetition reports its last iteration, and one that can match
	   nothing still takes part. */
	static const long last_iteration[] = { 0, 2, 1, 2 };
	match_is(REG_EXTENDED, "(a+|b)*", "ab", 0, 2, last_iteration);
	static const long empty_iteration[] = { 0, 0, 0, 0 };
	match_is(REG_EXTENDED, "(a*)*", "b", 0, 2, empty_iteration);

	/* Intervals, anchors and classes. */
	static const long three_as[] = { 0, 3 };
	match_is(REG_EXTENDED, "a{2,3}", "aaaa", 0, 1, three_as);
	static const long digits[] = { 2, 5 };
	match_is(REG_EXTENDED, "[[:digit:]]+", "ab123c", 0, 1, digits);
	static const long at_end[] = { 3, 6 };
	match_is(REG_EXTENDED, "cat|dog$", "hotdog", 0, 1, at_end);
	no_match(REG_EXTENDED, "^a", "ba", 0);

	/* A basic expression: the operators need their backslashes, and the
	   back-references only a BRE has. */
	static const long two_as[] = { 0, 2 };
	match_is(0, "a\\{2\\}", "aaa", 0, 1, two_as);
	static const long doubled[] = { 2, 4, 2, 3 };
	match_is(0, "\\(.\\)\\1", "abccd", 0, 2, doubled);
	static const long halves[] = { 0, 4, 0, 2 };
	match_is(0, "\\(a*\\)\\1", "aaaa", 0, 2, halves);
	/* libc-test's regex-backref-0: \0 is not a back-reference. */
	match_is(0, "a\\0", "a0", 0, 1, two_as);
	/* libc-test's regex-ere-backref: an ERE has none, so \1 is a 1. */
	no_match(REG_EXTENDED, "(a)\\1", "aa", 0);
	static const long one_two[] = { 0, 2, 0, 1 };
	match_is(REG_EXTENDED, "(a)\\1", "a1", 0, 2, one_two);

	/* REG_ICASE, including libc-test's regex-bracket-icase: the list is
	   folded before it is negated. */
	static const long shouting[] = { 1, 4 };
	match_is(REG_ICASE, "abc", "xABC", 0, 1, shouting);
	no_match(REG_ICASE, "[^aBcC]", "b", 0);
	no_match(REG_ICASE, "[^aBcC]", "C", 0);
	static const long first_byte[] = { 0, 1 };
	match_is(REG_ICASE, "[^aBcC]", "D", 0, 1, first_byte);
	/* libc-test's regex-negated-range: overlapping negated ranges. */
	no_match(0, "[^aa-z]", "k", 0);

	/* REG_NEWLINE: ^ and $ meet newlines, and neither . nor a negated
	   list matches one. */
	static const long after_newline[] = { 2, 3 };
	match_is(REG_NEWLINE, "^b", "a\nb", 0, 1, after_newline);
	no_match(0, "^b", "a\nb", 0);
	match_is(REG_NEWLINE, "a$", "a\nb", 0, 1, first_byte);
	no_match(REG_NEWLINE, ".", "\n", 0);
	no_match(REG_NEWLINE, "[^a]", "\n", 0);
	match_is(0, ".", "\n", 0, 1, first_byte);

	/* REG_NOTBOL and REG_NOTEOL. */
	no_match(0, "^a", "a", REG_NOTBOL);
	no_match(0, "a$", "a", REG_NOTEOL);
	match_is(REG_NEWLINE, "^a", "b\na", REG_NOTBOL, 1, after_newline);

	/* REG_NOSUB, and libc-test's regexec-nosub: a non-zero nmatch with no
	   array must not be written through. */
	CHECK(regcomp(&re, "abc", REG_NOSUB) == 0);
	CHECK(regexec(&re, "zyx abc", 1, 0, 0) == 0);
	CHECK(regexec(&re, "zyx", 1, 0, 0) == REG_NOMATCH);
	regfree(&re);

	/* More match entries than the pattern has groups: the rest are -1. */
	CHECK(regcomp(&re, "(a)(b)", REG_EXTENDED) == 0);
	regmatch_t five[5];
	CHECK(regexec(&re, "zab", 5, five, 0) == 0);
	CHECK(five[3].rm_so == -1 && five[3].rm_eo == -1);
	CHECK(five[4].rm_so == -1 && five[4].rm_eo == -1);
	regfree(&re);

	/* The error codes, with the messages musl gives. */
	bad_pattern(REG_EXTENDED, "(a", REG_EPAREN, "Missing ')'");
	bad_pattern(0, "\\(a", REG_EPAREN, "Missing ')'");
	bad_pattern(0, "a\\", REG_EESCAPE, "Trailing backslash");
	bad_pattern(0, "\\1", REG_ESUBREG, "Invalid back reference");
	bad_pattern(0, "[a", REG_EBRACK, "Missing ']'");
	bad_pattern(0, "[z-a]", REG_ERANGE, "Invalid character range");
	bad_pattern(0, "[[:bogus:]]", REG_ECTYPE, "Unknown character class name");
	bad_pattern(0, "[[.ab.]]", REG_ECOLLATE, "Unknown collating element");
	bad_pattern(REG_EXTENDED, "*a", REG_BADRPT,
	            "Repetition not preceded by valid expression");
	bad_pattern(REG_EXTENDED, "a{2,1}", REG_BADBR, "Invalid contents of {}");
	bad_pattern(REG_EXTENDED, "a{1", REG_EBRACE, "Missing '}'");

	/* regerror cuts the message to the room it is given, and returns the
	   room the whole message needs. */
	char small[5];
	size_t n = regerror(REG_NOMATCH, 0, small, sizeof small);
	CHECK(n == strlen("No match") + 1);
	CHECK(strcmp(small, "No m") == 0);
	CHECK(regerror(REG_NOMATCH, 0, 0, 0) == strlen("No match") + 1);

	return t_status;
}
