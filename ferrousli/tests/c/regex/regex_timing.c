/*
 * Patterns that make a backtracking matcher take exponential time. Each of
 * these must finish in well under a second here; the check allows ten, so
 * that an unoptimised build on a busy machine passes, while a matcher that
 * backtracks would never reach the end.
 */

#define _POSIX_C_SOURCE 200809L
#include <regex.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "check.h"

/* Seconds since some fixed point. */
static double now(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

/* Runs `pat` over `s` and checks that it does not match. */
static void no_match(int cflags, const char *pat, const char *s)
{
	regex_t re;
	if (regcomp(&re, pat, cflags) != 0) {
		CHECK(!"the pattern compiles");
		return;
	}
	CHECK(regexec(&re, s, 0, 0, 0) == REG_NOMATCH);
	regfree(&re);
}

int main(void)
{
	enum { LEN = 100000 };
	char *many = malloc(LEN + 2);
	CHECK(many != 0);
	if (!many)
		return t_status;
	memset(many, 'a', LEN);
	many[LEN] = 0;

	double started = now();
	no_match(REG_EXTENDED, "(a*)*b", many);
	no_match(REG_EXTENDED, "(a|aa)+c", many);
	no_match(REG_EXTENDED, "(a*)(a*)(a*)d", many);
	no_match(0, "\\(a*\\)*b", many);
	CHECK(now() - started < 10.0);

	/* The same patterns when they do match, with the groups reported. */
	many[LEN] = 'b';
	many[LEN + 1] = 0;
	regex_t re;
	regmatch_t m[2];
	started = now();
	CHECK(regcomp(&re, "(a*)*b", REG_EXTENDED) == 0);
	CHECK(regexec(&re, many, 2, m, 0) == 0);
	CHECK(m[0].rm_so == 0 && m[0].rm_eo == LEN + 1);
	CHECK(m[1].rm_so == 0 && m[1].rm_eo == LEN);
	regfree(&re);
	CHECK(now() - started < 10.0);

	/* A long run of alternating pairs, where each iteration of the group
	   has to be split off in turn. */
	char *pairs = malloc(LEN + 1);
	CHECK(pairs != 0);
	if (!pairs) {
		free(many);
		return t_status;
	}
	for (int i = 0; i < LEN; i += 2) {
		pairs[i] = 'a';
		pairs[i + 1] = 'b';
	}
	pairs[LEN] = 0;
	started = now();
	CHECK(regcomp(&re, "(ab|a)*", REG_EXTENDED) == 0);
	CHECK(regexec(&re, pairs, 2, m, 0) == 0);
	CHECK(m[0].rm_so == 0 && m[0].rm_eo == LEN);
	CHECK(m[1].rm_so == LEN - 2 && m[1].rm_eo == LEN);
	regfree(&re);
	CHECK(now() - started < 10.0);

	free(pairs);
	free(many);
	return t_status;
}
