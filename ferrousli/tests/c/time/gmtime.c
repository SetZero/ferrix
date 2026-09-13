/*
 * UTC calendar time: gmtime and timegm before 1970, after 2038, in year 0,
 * in negative years and at the edges of what tm_year holds; timegm
 * normalising fields out of range; difftime; asctime and ctime. Run with
 * TZ=UTC0.
 *
 * The rows were computed once with Python's datetime, shifting years outside
 * 1 to 9999 by 400-year cycles of 146097 days, and are written here
 * literally. main's second part is adapted from libc-test's
 * src/functional/time.c (MIT).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <stdlib.h>
#include <time.h>
#include "check.h"
#include "report.h"

struct row {
	long long t;
	int year, mon, mday, hour, min, sec, wday, yday;
};

static const struct row rows[] = {
	{0LL, 70, 0, 1, 0, 0, 0, 4, 0},
	{-1LL, 69, 11, 31, 23, 59, 59, 3, 364},
	{2147483647LL, 138, 0, 19, 3, 14, 7, 2, 18},
	{2147483648LL, 138, 0, 19, 3, 14, 8, 2, 18},
	{-2147483648LL, 1, 11, 13, 20, 45, 52, 5, 346},
	{-2203891200LL, 0, 2, 1, 0, 0, 0, 4, 59},             /* 1900-03-01 */
	{-11670955200LL, -300, 1, 29, 12, 0, 0, 2, 59},       /* 1600-02-29 */
	{253402300799LL, 8099, 11, 31, 23, 59, 59, 5, 364},   /* 9999-12-31 */
	{253402300800LL, 8100, 0, 1, 0, 0, 0, 6, 0},          /* 10000-01-01 */
	{-62135596801LL, -1900, 11, 31, 23, 59, 59, 0, 365},  /* 0000-12-31 */
	{-62162121600LL, -1900, 1, 29, 0, 0, 0, 2, 59},       /* 0000-02-29 */
	{-62167219200LL, -1900, 0, 1, 0, 0, 0, 6, 0},         /* 0000-01-01 */
	{-62167219201LL, -1901, 11, 31, 23, 59, 59, 5, 364},  /* -0001-12-31 */
	{-62198755200LL, -1901, 0, 1, 0, 0, 0, 5, 0},         /* -0001-01-01 */
	{-66048652800LL, -2023, 0, 1, 0, 0, 0, 1, 0},         /* -0123-01-01 */
	{67768036160140800LL, 2147483647, 0, 1, 0, 0, 0, 3, 0},
	{67768036191676799LL, 2147483647, 11, 31, 23, 59, 59, 3, 364},
	{-67768040609740800LL, -2147483648, 0, 1, 0, 0, 0, 4, 0},
};

static void check_row(const struct row *r)
{
	int before = t_status;
	time_t t = r->t;
	struct tm tm, *p;

	t_status = 0;
	errno = 0;
	p = gmtime(&t);
	CHECK(p != 0);
	CHECK(errno == 0);
	if (p) {
		CHECK(p->tm_year == r->year);
		CHECK(p->tm_mon == r->mon);
		CHECK(p->tm_mday == r->mday);
		CHECK(p->tm_hour == r->hour);
		CHECK(p->tm_min == r->min);
		CHECK(p->tm_sec == r->sec);
		CHECK(p->tm_wday == r->wday);
		CHECK(p->tm_yday == r->yday);
		CHECK(p->tm_isdst == 0 && p->tm_gmtoff == 0);
		CHECK(strcmp(p->tm_zone, "UTC") == 0);
		tm = *p;
		tm.tm_wday = tm.tm_yday = -1;
		CHECK(timegm(&tm) == t);
		CHECK(tm.tm_wday == r->wday && tm.tm_yday == r->yday);
	}
	CHECK(gmtime_r(&t, &tm) == &tm);
	CHECK(tm.tm_year == r->year && tm.tm_mday == r->mday);
	if (t_status) {
		say("  at ");
		say_number(r->t);
		say("\n");
	}
	t_status |= before;
}

struct normal {
	int year, mon, mday, hour, min, sec;
	long long t;
	int out_year, out_mon, out_mday, out_hour, out_min, out_sec, wday, yday;
};

static const struct normal normals[] = {
	/* 2024-13-00 -01:60:-3600 is 2024-12-30 23:00. */
	{124, 12, 0, -1, 60, -3600, 1735599600LL, 124, 11, 30, 23, 0, 0, 1, 364},
	/* Month 13 of 2023 is February 2024. */
	{123, 13, 1, 0, 0, 0, 1706745600LL, 124, 1, 1, 0, 0, 0, 4, 31},
	/* Day 0 of March in a leap year. */
	{124, 2, 0, 0, 0, 0, 1709164800LL, 124, 1, 29, 0, 0, 0, 4, 59},
	/* The 29th of February in a year without one. */
	{123, 1, 29, 0, 0, 0, 1677628800LL, 123, 2, 1, 0, 0, 0, 3, 59},
	/* Month -1. */
	{124, -1, 15, 0, 0, 0, 1702598400LL, 123, 11, 15, 0, 0, 0, 5, 348},
	/* Second 60. */
	{116, 11, 31, 23, 59, 60, 1483228800LL, 117, 0, 1, 0, 0, 0, 0, 0},
};

static void check_normal(const struct normal *n)
{
	int before = t_status;
	struct tm tm = {
		.tm_year = n->year, .tm_mon = n->mon, .tm_mday = n->mday,
		.tm_hour = n->hour, .tm_min = n->min, .tm_sec = n->sec,
		.tm_isdst = 1,
	};
	struct tm local = tm;

	t_status = 0;
	CHECK(timegm(&tm) == n->t);
	CHECK(tm.tm_year == n->out_year);
	CHECK(tm.tm_mon == n->out_mon);
	CHECK(tm.tm_mday == n->out_mday);
	CHECK(tm.tm_hour == n->out_hour);
	CHECK(tm.tm_min == n->out_min);
	CHECK(tm.tm_sec == n->out_sec);
	CHECK(tm.tm_wday == n->wday);
	CHECK(tm.tm_yday == n->yday);
	CHECK(tm.tm_isdst == 0);
	/* In UTC, mktime agrees, whatever tm_isdst said. */
	CHECK(mktime(&local) == n->t);
	CHECK(local.tm_mday == n->out_mday && local.tm_isdst == 0);
	if (t_status) {
		say("  normalising to ");
		say_number(n->t);
		say("\n");
	}
	t_status |= before;
}

/* libc-test: gmtime and mktime round trips in GMT. */
#define TM(ss, mm, hh, md, mo, yr, wd, yd, dst) (struct tm){ \
	.tm_sec = ss, .tm_min = mm, .tm_hour = hh,                \
	.tm_mday = md, .tm_mon = mo, .tm_year = yr,                \
	.tm_wday = wd, .tm_yday = yd, .tm_isdst = dst }

static int tm_cmp(struct tm a, struct tm b)
{
	return a.tm_sec != b.tm_sec || a.tm_min != b.tm_min ||
	       a.tm_hour != b.tm_hour || a.tm_mday != b.tm_mday ||
	       a.tm_mon != b.tm_mon || a.tm_year != b.tm_year ||
	       a.tm_wday != b.tm_wday || a.tm_yday != b.tm_yday ||
	       a.tm_isdst != b.tm_isdst;
}

static void tm2sec(struct tm tm)
{
	struct tm copy = tm;
	errno = 0;
	time_t t = mktime(&copy);
	CHECK(t != -1);
	CHECK(errno == 0);
	CHECK(tm_cmp(*gmtime(&t), tm) == 0);
}

static void sec2tm(time_t t)
{
	errno = 0;
	struct tm *tm = gmtime(&t);
	CHECK(errno == 0);
	CHECK(mktime(tm) == t);
	CHECK(errno == 0);
}

static char *mutable(const char *s)
{
	static char buf[16];
	strcpy(buf, s);
	return buf;
}

int main(void)
{
	time_t t;
	struct tm tm;
	char buf[26];

	for (size_t i = 0; i < sizeof rows / sizeof rows[0]; i++)
		check_row(&rows[i]);
	for (size_t i = 0; i < sizeof normals / sizeof normals[0]; i++)
		check_normal(&normals[i]);

	/* Beyond tm_year, both ways. */
	const long long beyond[] = {
		67768036191676800LL, -67768040609740801LL, LLONG_MAX, LLONG_MIN,
	};
	for (int i = 0; i < 4; i++) {
		t = beyond[i];
		errno = 0;
		CHECK(gmtime(&t) == 0);
		CHECK(errno == EOVERFLOW);
		errno = 0;
		CHECK(gmtime_r(&t, &tm) == 0);
		CHECK(errno == EOVERFLOW);
		errno = 0;
		CHECK(ctime_r(&t, buf) == 0);
		CHECK(errno == EOVERFLOW);
	}
	tm = (struct tm){ .tm_year = INT_MAX, .tm_mon = 12, .tm_mday = 1 };
	errno = 0;
	CHECK(timegm(&tm) == -1);
	CHECK(errno == EOVERFLOW);
	CHECK(tm.tm_mon == 12);

	/* difftime. */
	CHECK(difftime(1, 0) == 1.0);
	CHECK(difftime(0, 1) == -1.0);
	CHECK(difftime(LLONG_MAX, LLONG_MIN) == 18446744073709551616.0);

	/* asctime and ctime. */
	t = 0;
	CHECK(strcmp(asctime(gmtime(&t)), "Thu Jan  1 00:00:00 1970\n") == 0);
	CHECK(strcmp(ctime(&t), "Thu Jan  1 00:00:00 1970\n") == 0);
	CHECK(ctime_r(&t, buf) == buf);
	CHECK(strcmp(buf, "Thu Jan  1 00:00:00 1970\n") == 0);
	t = 2147483648LL;
	CHECK(asctime_r(gmtime(&t), buf) == buf);
	CHECK(strcmp(buf, "Tue Jan 19 03:14:08 2038\n") == 0);
	t = -66048652800LL;
	CHECK(strcmp(asctime(gmtime(&t)), "Mon Jan  1 00:00:00 -123\n") == 0);
	tm = (struct tm){ .tm_mday = 1, .tm_year = 70, .tm_wday = 7, .tm_mon = -1,
	                  .tm_hour = 5 };
	CHECK(asctime_r(&tm, buf) == buf);
	CHECK(strcmp(buf, "??? ???  1 05:00:00 1970\n") == 0);
	/* A negative hour makes the text 26 bytes before its NUL. */
	tm.tm_hour = -5;
	errno = 0;
	CHECK(asctime(&tm) == 0);
	CHECK(errno == EOVERFLOW);
	t = 253402300800LL;
	errno = 0;
	CHECK(asctime_r(gmtime(&t), buf) == 0);
	CHECK(errno == EOVERFLOW);
	CHECK(setenv("TZ", "CET-1CEST,M3.5.0,M10.5.0/3", 1) == 0);
	t = 1720000000;
	CHECK(strcmp(ctime(&t), "Wed Jul  3 11:46:40 2024\n") == 0);

	/* libc-test's time.c. */
	CHECK(putenv(mutable("TZ=GMT")) == 0);
	tzset();
	tm2sec(TM(0, 0, 0, 1, 0, 70, 4, 0, 0));
	tm2sec(TM(7, 14, 3, 19, 0, 138, 2, 18, 0));
	tm2sec(TM(8, 14, 3, 19, 0, 138, 2, 18, 0));
	sec2tm(0);
	for (t = 1; t < 1000; t++)
		sec2tm(t * 100003);
	return t_status;
}
