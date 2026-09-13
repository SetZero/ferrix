/*
 * Local time: POSIX TZ strings and TZif zones from the host's
 * /usr/share/zoneinfo, each at both edges of its transitions; tzname,
 * timezone and daylight; mktime reading every row back; mktime's reading of
 * tm_isdst in the skipped and the repeated hour; overflow; and what an unset,
 * empty or unusable TZ, and TZDIR, select.
 *
 * The broken-down times in `rows` were printed once by the host's glibc
 * (tzdata 2026c) with localtime_r, and the POSIX rule edges checked against
 * Python's datetime; they are written here literally.
 *
 * tzname[1] is "" for a zone without daylight saving time now, and a zone
 * file's tzname and daylight come from its footer, as in musl. glibc repeats
 * a name there, and takes daylight from the zone's history.
 *
 * The mktime results in `makes` follow musl's algorithm, worked by hand: the
 * wall-clock time is looked up as local time, where a skipped or repeated
 * time reads as daylight saving time; when tm_isdst asks for the other kind,
 * the difference between the offsets is applied.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <stdlib.h>
#include <time.h>
#include "check.h"
#include "report.h"

#define CET "CET-1CEST,M3.5.0,M10.5.0/3"
#define HOWE "<+1030>-10:30<+11>-11,M10.1.0,M4.1.0"

struct row {
	const char *tz;
	long long t;
	int year, mon, mday, hour, min, sec, wday, yday, isdst;
	long gmtoff;
	const char *zone;
};

static const struct row rows[] = {
	{"UTC0", 0LL, 70, 0, 1, 0, 0, 0, 4, 0, 0, 0, "UTC"},
	{"UTC0", 1720000000LL, 124, 6, 3, 9, 46, 40, 3, 184, 0, 0, "UTC"},
	{"UTC0", -1LL, 69, 11, 31, 23, 59, 59, 3, 364, 0, 0, "UTC"},
	{"<+0330>-3:30", 0LL, 70, 0, 1, 3, 30, 0, 4, 0, 0, 12600, "+0330"},
	{"<+0330>-3:30", 1720000000LL, 124, 6, 3, 13, 16, 40, 3, 184, 0, 12600, "+0330"},
	/*
	 * No rules: the United States' since 2007. The host's glibc put the
	 * end an hour early, at 05:00 UTC; the third row follows the rule,
	 * 02:00 EDT, as Python's America/New_York does.
	 */
	{"EST5EDT", 1710053999LL, 124, 2, 10, 1, 59, 59, 0, 69, 0, -18000, "EST"},
	{"EST5EDT", 1710054000LL, 124, 2, 10, 3, 0, 0, 0, 69, 1, -14400, "EDT"},
	{"EST5EDT", 1730613599LL, 124, 10, 3, 1, 59, 59, 0, 307, 1, -14400, "EDT"},
	{"EST5EDT", 1730613600LL, 124, 10, 3, 1, 0, 0, 0, 307, 0, -18000, "EST"},
	{CET, 1711846799LL, 124, 2, 31, 1, 59, 59, 0, 90, 0, 3600, "CET"},
	{CET, 1711846800LL, 124, 2, 31, 3, 0, 0, 0, 90, 1, 7200, "CEST"},
	{CET, 1729990799LL, 124, 9, 27, 2, 59, 59, 0, 300, 1, 7200, "CEST"},
	{CET, 1729990800LL, 124, 9, 27, 2, 0, 0, 0, 300, 0, 3600, "CET"},
	/* The southern hemisphere, and a 30-minute change. */
	{HOWE, 1712415599LL, 124, 3, 7, 1, 59, 59, 0, 97, 1, 39600, "+11"},
	{HOWE, 1712415600LL, 124, 3, 7, 1, 30, 0, 0, 97, 0, 37800, "+1030"},
	{HOWE, 1728142199LL, 124, 9, 6, 1, 59, 59, 0, 279, 0, 37800, "+1030"},
	{HOWE, 1728142200LL, 124, 9, 6, 2, 30, 0, 0, 279, 1, 39600, "+11"},
	{HOWE, 1704067200LL, 124, 0, 1, 11, 0, 0, 1, 0, 1, 39600, "+11"},
	/* A negative transition time, and one beyond 24 hours. */
	{"EST5EDT,M3.2.0/-1,M11.1.0/49", 1710043199LL, 124, 2, 9, 22, 59, 59, 6, 68, 0, -18000, "EST"},
	{"EST5EDT,M3.2.0/-1,M11.1.0/49", 1710043200LL, 124, 2, 10, 0, 0, 0, 0, 69, 1, -14400, "EDT"},
	{"EST5EDT,M3.2.0/-1,M11.1.0/49", 1730782799LL, 124, 10, 5, 0, 59, 59, 2, 309, 1, -14400, "EDT"},
	{"EST5EDT,M3.2.0/-1,M11.1.0/49", 1730782800LL, 124, 10, 5, 0, 0, 0, 2, 309, 0, -18000, "EST"},
	/* Jn skips the 29th of February; n counts it. */
	{"AAA3BBB,J60/-1,300/26", 1709258399LL, 124, 1, 29, 22, 59, 59, 4, 59, 0, -10800, "AAA"},
	{"AAA3BBB,J60/-1,300/26", 1709258400LL, 124, 2, 1, 0, 0, 0, 5, 60, 1, -7200, "BBB"},
	{"AAA3BBB,J60/-1,300/26", 1730087999LL, 124, 9, 28, 1, 59, 59, 1, 301, 1, -7200, "BBB"},
	{"AAA3BBB,J60/-1,300/26", 1730088000LL, 124, 9, 28, 1, 0, 0, 1, 301, 0, -10800, "AAA"},
	{"AAA3BBB,J60/-1,300/26", 1740794399LL, 125, 1, 28, 22, 59, 59, 5, 58, 0, -10800, "AAA"},
	{"AAA3BBB,J60/-1,300/26", 1740794400LL, 125, 2, 1, 0, 0, 0, 6, 59, 1, -7200, "BBB"},
	{"AAA3BBB,J60/-1,300/26", 1761710399LL, 125, 9, 29, 1, 59, 59, 3, 301, 1, -7200, "BBB"},
	{"AAA3BBB,J60/-1,300/26", 1761710400LL, 125, 9, 29, 1, 0, 0, 3, 301, 0, -10800, "AAA"},
	/* Local mean time with seconds before 1893, then the footer. */
	{"Europe/Berlin", -2422054409LL, -7, 2, 31, 23, 59, 59, 5, 89, 0, 3208, "LMT"},
	{"Europe/Berlin", -2422054408LL, -7, 3, 1, 0, 6, 32, 6, 90, 0, 3600, "CET"},
	{":Europe/Berlin", 1711846799LL, 124, 2, 31, 1, 59, 59, 0, 90, 0, 3600, "CET"},
	{":Europe/Berlin", 1711846800LL, 124, 2, 31, 3, 0, 0, 0, 90, 1, 7200, "CEST"},
	{"Europe/Berlin", 1729990799LL, 124, 9, 27, 2, 59, 59, 0, 300, 1, 7200, "CEST"},
	{"Europe/Berlin", 1729990800LL, 124, 9, 27, 2, 0, 0, 0, 300, 0, 3600, "CET"},
	{"Europe/Berlin", 2145916800LL, 138, 0, 1, 1, 0, 0, 5, 0, 0, 3600, "CET"},
	{"Europe/Berlin", 4102444800LL, 200, 0, 1, 1, 0, 0, 5, 0, 0, 3600, "CET"},
	{"America/New_York", -2717650801LL, -17, 10, 18, 12, 3, 57, 0, 321, 0, -17762, "LMT"},
	{"America/New_York", -2717650800LL, -17, 10, 18, 12, 0, 0, 0, 321, 0, -18000, "EST"},
	{"America/New_York", 1710054000LL, 124, 2, 10, 3, 0, 0, 0, 69, 1, -14400, "EDT"},
	{"America/New_York", 1730613600LL, 124, 10, 3, 1, 0, 0, 0, 307, 0, -18000, "EST"},
	{"America/New_York", -86400LL, 69, 11, 30, 19, 0, 0, 2, 363, 0, -18000, "EST"},
	{"America/New_York", 32503680000LL, 1099, 11, 31, 19, 0, 0, 2, 364, 0, -18000, "EST"},
	{"Australia/Lord_Howe", 1712415599LL, 124, 3, 7, 1, 59, 59, 0, 97, 1, 39600, "+11"},
	{"Australia/Lord_Howe", 1712415600LL, 124, 3, 7, 1, 30, 0, 0, 97, 0, 37800, "+1030"},
	{"Australia/Lord_Howe", 1728142199LL, 124, 9, 6, 1, 59, 59, 0, 279, 0, 37800, "+1030"},
	{"Australia/Lord_Howe", 1728142200LL, 124, 9, 6, 2, 30, 0, 0, 279, 1, 39600, "+11"},
	{"Australia/Lord_Howe", 2000000000LL, 133, 4, 18, 14, 3, 20, 3, 137, 0, 37800, "+1030"},
	{"Asia/Kathmandu", 504901799LL, 85, 11, 31, 23, 59, 59, 2, 364, 0, 19800, "+0530"},
	{"Asia/Kathmandu", 504901800LL, 86, 0, 1, 0, 15, 0, 3, 0, 0, 20700, "+0545"},
	{"Asia/Kathmandu", 0LL, 70, 0, 1, 5, 30, 0, 4, 0, 0, 19800, "+0530"},
	{"Asia/Kathmandu", -1577513600LL, 20, 0, 5, 23, 16, 40, 1, 4, 0, 19800, "+0530"},
	{"Asia/Kathmandu", -2000000000LL, 6, 7, 17, 2, 7, 56, 5, 228, 0, 20476, "LMT"},
	/* Daylight saving time until 2019, then the footer's <-03>3. */
	{"America/Sao_Paulo", 1541300399LL, 118, 10, 3, 23, 59, 59, 6, 306, 0, -10800, "-03"},
	{"America/Sao_Paulo", 1541300400LL, 118, 10, 4, 1, 0, 0, 0, 307, 1, -7200, "-02"},
	{"America/Sao_Paulo", 1550368799LL, 119, 1, 16, 23, 59, 59, 6, 46, 1, -7200, "-02"},
	{"America/Sao_Paulo", 1550368800LL, 119, 1, 16, 23, 0, 0, 6, 46, 0, -10800, "-03"},
	{"America/Sao_Paulo", 1900000000LL, 130, 2, 17, 14, 46, 40, 0, 75, 0, -10800, "-03"},
	{"America/Sao_Paulo", 4000000000LL, 196, 9, 2, 4, 6, 40, 2, 275, 0, -10800, "-03"},
};

struct globals {
	const char *tz, *std, *dst;
	long timezone;
	int daylight;
};

static const struct globals globals[] = {
	{"UTC0", "UTC", "", 0, 0},
	{"<+0330>-3:30", "+0330", "", -12600, 0},
	{"EST5EDT", "EST", "EDT", 18000, 1},
	{CET, "CET", "CEST", -3600, 1},
	{HOWE, "+1030", "+11", -37800, 1},
	{"AAA3BBB,J60/-1,300/26", "AAA", "BBB", 10800, 1},
	{"Europe/Berlin", "CET", "CEST", -3600, 1},
	{"America/New_York", "EST", "EDT", 18000, 1},
	{"Australia/Lord_Howe", "+1030", "+11", -37800, 1},
	{"Asia/Kathmandu", "+0545", "", -20700, 0},
	{"America/Sao_Paulo", "-03", "", 10800, 0},
};

struct make {
	const char *tz;
	int year, mon, mday, hour, min, sec, isdst;
	long long t;
	int out_mon, out_mday, out_hour, out_min, out_isdst;
	long out_gmtoff;
};

static const struct make makes[] = {
	/* 02:30 on 2024-03-31 does not exist in Berlin. */
	{CET, 124, 2, 31, 2, 30, 0, -1, 1711845000LL, 2, 31, 1, 30, 0, 3600},
	{CET, 124, 2, 31, 2, 30, 0, 1, 1711845000LL, 2, 31, 1, 30, 0, 3600},
	{CET, 124, 2, 31, 2, 30, 0, 0, 1711848600LL, 2, 31, 3, 30, 1, 7200},
	{"Europe/Berlin", 124, 2, 31, 2, 30, 0, -1, 1711845000LL, 2, 31, 1, 30, 0, 3600},
	{"Europe/Berlin", 124, 2, 31, 2, 30, 0, 0, 1711848600LL, 2, 31, 3, 30, 1, 7200},
	/* 02:30 on 2024-10-27 happens twice. */
	{CET, 124, 9, 27, 2, 30, 0, -1, 1729989000LL, 9, 27, 2, 30, 1, 7200},
	{CET, 124, 9, 27, 2, 30, 0, 1, 1729989000LL, 9, 27, 2, 30, 1, 7200},
	{CET, 124, 9, 27, 2, 30, 0, 0, 1729992600LL, 9, 27, 2, 30, 0, 3600},
	{"Europe/Berlin", 124, 9, 27, 2, 30, 0, 0, 1729992600LL, 9, 27, 2, 30, 0, 3600},
	/* Fields out of range, landing in the skipped hour. */
	{"Europe/Berlin", 124, 2, 30, 26, 30, 0, -1, 1711845000LL, 2, 31, 1, 30, 0, 3600},
	/* Asking for standard time in summer moves the time an hour on. */
	{CET, 124, 6, 3, 11, 46, 40, -1, 1720000000LL, 6, 3, 11, 46, 1, 7200},
	{CET, 124, 6, 3, 11, 46, 40, 0, 1720003600LL, 6, 3, 12, 46, 1, 7200},
	/* New York's skipped 02:30 and repeated 01:30. */
	{"America/New_York", 124, 2, 10, 2, 30, 0, -1, 1710052200LL, 2, 10, 1, 30, 0, -18000},
	{"America/New_York", 124, 2, 10, 2, 30, 0, 0, 1710055800LL, 2, 10, 3, 30, 1, -14400},
	{"America/New_York", 124, 2, 10, 2, 30, 0, 1, 1710052200LL, 2, 10, 1, 30, 0, -18000},
	{"EST5EDT", 124, 10, 3, 1, 30, 0, -1, 1730611800LL, 10, 3, 1, 30, 1, -14400},
	{"America/New_York", 124, 10, 3, 1, 30, 0, 1, 1730611800LL, 10, 3, 1, 30, 1, -14400},
	{"America/New_York", 124, 10, 3, 1, 30, 0, 0, 1730615400LL, 10, 3, 1, 30, 0, -18000},
	/* Lord Howe skips 02:00 to 02:30 only. */
	{"Australia/Lord_Howe", 124, 9, 6, 2, 15, 0, -1, 1728141300LL, 9, 6, 1, 45, 0, 37800},
	{"Australia/Lord_Howe", 124, 9, 6, 2, 15, 0, 0, 1728143100LL, 9, 6, 2, 45, 1, 39600},
	{HOWE, 124, 9, 6, 2, 15, 0, 0, 1728143100LL, 9, 6, 2, 45, 1, 39600},
};

static void use_zone(const char *tz)
{
	CHECK(setenv("TZ", tz, 1) == 0);
	tzset();
}

static void context(const char *tz, long long t, int before)
{
	if (t_status) {
		say("  with TZ=");
		say(tz);
		say(" at ");
		say_number(t);
		say("\n");
	}
	t_status |= before;
}

static void check_row(const struct row *r)
{
	int before = t_status;
	time_t t = r->t;
	struct tm tm;

	t_status = 0;
	use_zone(r->tz);
	CHECK(localtime_r(&t, &tm) == &tm);
	CHECK(tm.tm_year == r->year);
	CHECK(tm.tm_mon == r->mon);
	CHECK(tm.tm_mday == r->mday);
	CHECK(tm.tm_hour == r->hour);
	CHECK(tm.tm_min == r->min);
	CHECK(tm.tm_sec == r->sec);
	CHECK(tm.tm_wday == r->wday);
	CHECK(tm.tm_yday == r->yday);
	CHECK(tm.tm_isdst == r->isdst);
	CHECK(tm.tm_gmtoff == r->gmtoff);
	CHECK(strcmp(tm.tm_zone, r->zone) == 0);

	/*
	 * mktime reads every row back, including the repeated hour, where
	 * tm_isdst tells the two apart. New York's first second of standard
	 * time repeats a wall-clock time of local mean time, 3 minutes 58
	 * seconds earlier, with tm_isdst 0 on both sides; mktime takes the
	 * earlier one.
	 */
	struct tm copy = tm;
	long long back = r->t == -2717650800LL ? r->t - 238 : r->t;
	CHECK(mktime(&copy) == back);
	CHECK(copy.tm_hour == tm.tm_hour && copy.tm_mday == tm.tm_mday);
	CHECK(copy.tm_min == tm.tm_min && copy.tm_sec == tm.tm_sec);
	CHECK(copy.tm_isdst == tm.tm_isdst);
	CHECK(copy.tm_gmtoff == (back == r->t ? tm.tm_gmtoff : -17762));
	context(r->tz, r->t, before);
}

static void check_globals(const struct globals *g)
{
	int before = t_status;

	t_status = 0;
	use_zone(g->tz);
	CHECK(strcmp(tzname[0], g->std) == 0);
	CHECK(strcmp(tzname[1], g->dst) == 0);
	CHECK(timezone == g->timezone);
	CHECK(daylight == g->daylight);
	context(g->tz, 0, before);
}

static void check_make(const struct make *m)
{
	int before = t_status;
	struct tm tm = {
		.tm_year = m->year, .tm_mon = m->mon, .tm_mday = m->mday,
		.tm_hour = m->hour, .tm_min = m->min, .tm_sec = m->sec,
		.tm_isdst = m->isdst,
	};

	t_status = 0;
	use_zone(m->tz);
	errno = 0;
	CHECK(mktime(&tm) == m->t);
	CHECK(errno == 0);
	CHECK(tm.tm_mon == m->out_mon);
	CHECK(tm.tm_mday == m->out_mday);
	CHECK(tm.tm_hour == m->out_hour);
	CHECK(tm.tm_min == m->out_min);
	CHECK(tm.tm_isdst == m->out_isdst);
	CHECK(tm.tm_gmtoff == m->out_gmtoff);
	context(m->tz, m->t, before);
}

/* The zone a TZ gives at 2024-07-03 09:46:40 UTC. */
static void expect_zone(const char *tz, long gmtoff, const char *zone)
{
	int before = t_status;
	time_t t = 1720000000;
	struct tm tm;

	t_status = 0;
	use_zone(tz);
	CHECK(localtime_r(&t, &tm) == &tm);
	CHECK(tm.tm_gmtoff == gmtoff);
	CHECK(strcmp(tm.tm_zone, zone) == 0);
	CHECK(strcmp(zone, "UTC") != 0 || strcmp(tzname[0], "UTC") == 0);
	context(tz, t, before);
}

static void defaults(void)
{
	time_t t = 1720000000;
	struct tm unset, path;

	/* Unset is /etc/localtime, whatever the host has there. */
	CHECK(unsetenv("TZ") == 0);
	tzset();
	CHECK(localtime_r(&t, &unset) == &unset);
	const char *unset_name = tzname[0];
	use_zone("/etc/localtime");
	CHECK(localtime_r(&t, &path) == &path);
	CHECK(unset.tm_gmtoff == path.tm_gmtoff);
	CHECK(unset.tm_hour == path.tm_hour);
	CHECK(strcmp(unset.tm_zone, path.tm_zone) == 0);
	CHECK(strcmp(unset_name, tzname[0]) == 0);

	/* Empty, missing, malformed and refused names are UTC, as in musl. */
	expect_zone("", 0, "UTC");
	expect_zone("Nowhere/Zone", 0, "UTC");
	expect_zone("EST5EDT,M3", 0, "UTC");
	expect_zone("EST5EDT,M3.2.0,M11.1.0junk", 0, "UTC");
	expect_zone(":", 0, "UTC");
	expect_zone("Europe/../Europe/Berlin", 0, "UTC");
	expect_zone("./Europe/Berlin", 0, "UTC");
	expect_zone("<+03", 0, "UTC");
	CHECK(daylight == 0 && timezone == 0);

	/* A name and a path to the same file. */
	expect_zone(":UTC", 0, "UTC");
	expect_zone("GMT", 0, "GMT");
	expect_zone("Europe/Berlin", 7200, "CEST");
	expect_zone("/usr/share/zoneinfo/Europe/Berlin", 7200, "CEST");

	/* TZDIR replaces the standard directories. */
	CHECK(setenv("TZDIR", "/usr/share/zoneinfo/America", 1) == 0);
	expect_zone("New_York", -14400, "EDT");
	expect_zone("Europe/Berlin", 0, "UTC");
	CHECK(unsetenv("TZDIR") == 0);

	/*
	 * The zone is read again only when TZ changes, as in musl and glibc,
	 * so TZDIR's removal shows under another spelling of the same zone.
	 */
	expect_zone("Europe/Berlin", 0, "UTC");
	expect_zone(":Europe/Berlin", 7200, "CEST");

	/* A zone name outlives a change of zone. */
	struct tm berlin;
	use_zone("Europe/Berlin");
	CHECK(localtime_r(&t, &berlin) == &berlin);
	use_zone("America/New_York");
	CHECK(localtime_r(&t, &path) == &path);
	CHECK(strcmp(berlin.tm_zone, "CEST") == 0);
	CHECK(strcmp(path.tm_zone, "EDT") == 0);
}

static void overflow(void)
{
	/* 2147485547-12-31 23:59:59 UTC, the last second tm_year can hold. */
	time_t last = 67768036191676799LL, t;
	struct tm tm;

	use_zone("UTC0");
	CHECK(localtime_r(&last, &tm) == &tm);
	CHECK(tm.tm_year == INT_MAX && tm.tm_mon == 11 && tm.tm_sec == 59);

	/* An hour east of UTC, it is the next year. */
	use_zone("Europe/Berlin");
	errno = 0;
	CHECK(localtime_r(&last, &tm) == 0);
	CHECK(errno == EOVERFLOW);
	for (int i = 0; i < 2; i++) {
		t = i ? LLONG_MIN : LLONG_MAX;
		errno = 0;
		CHECK(localtime(&t) == 0);
		CHECK(errno == EOVERFLOW);
	}

	struct tm top = {
		.tm_year = INT_MAX, .tm_mon = 11, .tm_mday = 31,
		.tm_hour = 23, .tm_min = 59, .tm_sec = 59, .tm_isdst = -1,
	};
	struct tm past = top;
	CHECK(mktime(&top) == last - 3600);
	CHECK(top.tm_year == INT_MAX && top.tm_isdst == 0);
	past.tm_mday = 32;
	errno = 0;
	CHECK(mktime(&past) == -1);
	CHECK(errno == EOVERFLOW);
	CHECK(past.tm_mday == 32);
}

int main(void)
{
	for (size_t i = 0; i < sizeof rows / sizeof rows[0]; i++)
		check_row(&rows[i]);
	for (size_t i = 0; i < sizeof globals / sizeof globals[0]; i++)
		check_globals(&globals[i]);
	for (size_t i = 0; i < sizeof makes / sizeof makes[0]; i++)
		check_make(&makes[i]);
	defaults();
	overflow();
	return t_status;
}
