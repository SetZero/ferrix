/*
 * strftime: every conversion, the E and O modifiers, the padding and case
 * flags, widths, a buffer too small, and strftime_l.
 *
 * The first part is adapted from libc-test's src/functional/strftime.c (MIT).
 * The tables after it were printed once by the host's glibc and are written
 * here literally; musl and glibc agree on each. Where they differ, the
 * expectation is musl's and says so.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <stdlib.h>
#include <time.h>
#include "check.h"
#include "report.h"

#define CET "CET-1CEST,M3.5.0,M10.5.0/3"

static char buffer[1000];

static void check_at(int line, const char *format, const struct tm *tm, const char *expected)
{
	size_t len = strftime(buffer, sizeof buffer, format, tm);
	if (len != strlen(expected) || strcmp(buffer, expected) != 0) {
		say("strftime.c:");
		say_number(line);
		say(": \"");
		say(format);
		say("\": expected \"");
		say(expected);
		say("\", got \"");
		say(len ? buffer : "(nothing)");
		say("\"\n");
		t_status = 1;
	}
}

#define checkStrftime(f, tm, e) check_at(__LINE__, f, tm, e)

static void check_time(int line, const char *format, time_t t, const char *expected)
{
	struct tm tm;
	CHECK(localtime_r(&t, &tm) == &tm);
	check_at(line, format, &tm, expected);
}

#define AT(f, t, e) check_time(__LINE__, f, t, e)

static struct tm tm1 = {
	.tm_sec = 45, .tm_min = 23, .tm_hour = 13, .tm_mday = 3, .tm_mon = 0,
	.tm_year = 2016 - 1900, .tm_wday = 0, .tm_yday = 2, .tm_isdst = 0
};

static struct tm tm2 = {
	.tm_sec = 53, .tm_min = 17, .tm_hour = 5, .tm_mday = 5, .tm_mon = 0,
	.tm_year = 10009 - 1900, .tm_wday = 1, .tm_yday = 4, .tm_isdst = 0
};

static struct tm tm3 = {
	.tm_sec = 0, .tm_min = 0, .tm_hour = 12, .tm_mday = 23, .tm_mon = 1,
	.tm_year = 0 - 1900, .tm_wday = 3, .tm_yday = 53, .tm_isdst = 0
};

static struct tm tm4 = {
	.tm_sec = 0, .tm_min = 0, .tm_hour = 0, .tm_mday = 1, .tm_mon = 0,
	.tm_year = -123 - 1900, .tm_wday = 1, .tm_yday = 0, .tm_isdst = 0
};

static struct tm tm5 = {
	.tm_sec = 0, .tm_min = 0, .tm_hour = 0, .tm_mday = 1, .tm_mon = 0,
	.tm_year = INT_MAX, .tm_wday = 3, .tm_yday = 0, .tm_isdst = 0
};

static void libc_test(void)
{
	CHECK(setenv("TZ", "UTC0", 1) == 0);

	checkStrftime("%c", &tm1, "Sun Jan  3 13:23:45 2016");
	checkStrftime("%c", &tm2, "Mon Jan  5 05:17:53 +10009");
	checkStrftime("%c", &tm3, "Wed Feb 23 12:00:00 0000");

	checkStrftime("%C", &tm1, "20");
	checkStrftime("%03C", &tm1, "020");
	checkStrftime("%+3C", &tm1, "+20");
	checkStrftime("%C", &tm2, "100");
	checkStrftime("%C", &tm3, "00");
	checkStrftime("%01C", &tm3, "0");

	checkStrftime("%F", &tm1, "2016-01-03");
	checkStrftime("%012F", &tm1, "002016-01-03");
	checkStrftime("%+10F", &tm1, "2016-01-03");
	checkStrftime("%+11F", &tm1, "+2016-01-03");
	checkStrftime("%F", &tm2, "+10009-01-05");
	checkStrftime("%011F", &tm2, "10009-01-05");
	checkStrftime("%F", &tm3, "0000-02-23");
	checkStrftime("%01F", &tm3, "0-02-23");
	checkStrftime("%06F", &tm3, "0-02-23");
	checkStrftime("%010F", &tm3, "0000-02-23");
	checkStrftime("%F", &tm4, "-123-01-01");
	checkStrftime("%011F", &tm4, "-0123-01-01");

	checkStrftime("%g", &tm1, "15");
	checkStrftime("%g", &tm2, "09");

	checkStrftime("%G", &tm1, "2015");
	checkStrftime("%+5G", &tm1, "+2015");
	checkStrftime("%04G", &tm2, "10009");

	checkStrftime("%r", &tm1, "01:23:45 PM");
	checkStrftime("%r", &tm2, "05:17:53 AM");
	checkStrftime("%r", &tm3, "12:00:00 PM");
	checkStrftime("%r", &tm4, "12:00:00 AM");

	checkStrftime("%s", &tm1, "1451827425");
	checkStrftime("%s", &tm2, "253686748673");

	checkStrftime("%T", &tm1, "13:23:45");
	checkStrftime("%T", &tm2, "05:17:53");
	checkStrftime("%T", &tm3, "12:00:00");
	checkStrftime("%T", &tm4, "00:00:00");

	checkStrftime("%U", &tm1, "01");
	checkStrftime("%U", &tm2, "01");
	checkStrftime("%U", &tm3, "08");

	checkStrftime("%V", &tm1, "53");
	checkStrftime("%V", &tm2, "02");
	checkStrftime("%V", &tm3, "08");

	checkStrftime("%W", &tm1, "00");
	checkStrftime("%W", &tm2, "01");
	checkStrftime("%W", &tm3, "08");

	checkStrftime("%x", &tm1, "01/03/16");
	checkStrftime("%X", &tm1, "13:23:45");
	checkStrftime("%y", &tm1, "16");

	checkStrftime("%Y", &tm1, "2016");
	checkStrftime("%05Y", &tm1, "02016");
	checkStrftime("%+4Y", &tm1, "2016");
	checkStrftime("%+5Y", &tm1, "+2016");
	checkStrftime("%Y", &tm2, "+10009");
	checkStrftime("%05Y", &tm2, "10009");
	checkStrftime("%Y", &tm3, "0000");
	checkStrftime("%02Y", &tm3, "00");
	checkStrftime("%+5Y", &tm3, "+0000");
	checkStrftime("%Y", &tm4, "-123");
	checkStrftime("%+4Y", &tm4, "-123");
	checkStrftime("%+5Y", &tm4, "-0123");

	checkStrftime("%y", &tm5, "47");
	checkStrftime("%Y", &tm5, "+2147485547");
	checkStrftime("%011Y", &tm5, "02147485547");
	checkStrftime("%s", &tm5, "67768036160140800");
}

#define ALL "%a|%A|%b|%B|%c|%C|%d|%D|%e|%F|%g|%G|%h|%H|%I|%j|%k|%l|%m|%M|%n|%p|%P|%r|%R|%s|%S|%t|%T|%u|%U|%V|%w|%W|%x|%X|%y|%Y|%z|%Z|%%"
#define MODIFIED "%Ec|%EC|%Ex|%EX|%Ey|%EY|%Od|%Oe|%OH|%OI|%Om|%OM|%OS|%Ou|%OU|%OV|%Ow|%OW|%Oy"
#define FLAGS "%-d|%_d|%0e|%-e|%^a|%^B|%#a|%#b|%#p|%#Z|%^p|%_H|%-H|%-j|%_j|%^c|%-k|%0k|%_m"
#define WEEKS "%G-%V-%u %g %U %W %j"
#define HOURS "%I %l %p %P|%r"

static void tables(void)
{
	CHECK(setenv("TZ", CET, 1) == 0);
	AT(ALL, 1720000000, "Wed|Wednesday|Jul|July|Wed Jul  3 11:46:40 2024|20|03|07/03/24| 3|2024-07-03|24|2024|Jul|11|11|185|11|11|07|46|\n|AM|am|11:46:40 AM|11:46|1720000000|40|\t|11:46:40|3|26|27|3|27|07/03/24|11:46:40|24|2024|+0200|CEST|%");
	AT(ALL, 1704067200, "Mon|Monday|Jan|January|Mon Jan  1 01:00:00 2024|20|01|01/01/24| 1|2024-01-01|24|2024|Jan|01|01|001| 1| 1|01|00|\n|AM|am|01:00:00 AM|01:00|1704067200|00|\t|01:00:00|1|00|01|1|01|01/01/24|01:00:00|24|2024|+0100|CET|%");
	AT(ALL, 1735603200, "Tue|Tuesday|Dec|December|Tue Dec 31 01:00:00 2024|20|31|12/31/24|31|2024-12-31|25|2025|Dec|01|01|366| 1| 1|12|00|\n|AM|am|01:00:00 AM|01:00|1735603200|00|\t|01:00:00|2|52|01|2|53|12/31/24|01:00:00|24|2024|+0100|CET|%");
	AT(ALL, 1709255400, "Fri|Friday|Mar|March|Fri Mar  1 02:10:00 2024|20|01|03/01/24| 1|2024-03-01|24|2024|Mar|02|02|061| 2| 2|03|10|\n|AM|am|02:10:00 AM|02:10|1709255400|00|\t|02:10:00|5|08|09|5|09|03/01/24|02:10:00|24|2024|+0100|CET|%");
	AT(MODIFIED, 1720000000, "Wed Jul  3 11:46:40 2024|20|07/03/24|11:46:40|24|2024|03| 3|11|11|07|46|40|3|26|27|3|27|24");
	AT(FLAGS, 1720000000, "3| 3|03|3|WED|JULY|WED|JUL|am|cest|AM|11|11|185|185|WED JUL  3 11:46:40 2024|11|11| 7");
	AT(FLAGS, 1704067200, "1| 1|01|1|MON|JANUARY|MON|JAN|am|cet|AM| 1|1|1|  1|MON JAN  1 01:00:00 2024|1|01| 1");

	CHECK(setenv("TZ", "UTC0", 1) == 0);
	AT(WEEKS, 1451779200, "2015-53-7 15 01 00 003");
	AT(WEEKS, 1451865600, "2016-01-1 16 01 01 004");
	AT(WEEKS, 1230508800, "2009-01-1 09 52 52 364");
	AT(WEEKS, 1262476800, "2009-53-7 09 01 00 003");
	AT(WEEKS, 1609372800, "2020-53-4 20 52 52 366");
	AT(WEEKS, 1609459200, "2020-53-5 20 00 00 001");
	AT(WEEKS, 1735516800, "2025-01-1 25 52 53 365");
	AT(WEEKS, 1293753600, "2010-52-5 10 52 52 365");
	AT(WEEKS, 1293840000, "2010-52-6 10 00 00 001");
	AT(WEEKS, 1104537600, "2004-53-6 04 00 00 001");
	AT(HOURS, 0, "12 12 AM am|12:00:00 AM");
	AT(HOURS, 43200, "12 12 PM pm|12:00:00 PM");
	AT(HOURS, 82800, "11 11 PM pm|11:00:00 PM");
	AT(HOURS, 3600, "01  1 AM am|01:00:00 AM");
	AT(HOURS, 46800, "01  1 PM pm|01:00:00 PM");
	AT("%Z %z", 0, "UTC +0000");

	CHECK(setenv("TZ", "<+0330>-3:30", 1) == 0);
	AT("%z %Z", 0, "+0330 +0330");
	CHECK(setenv("TZ", "<-0545>5:45", 1) == 0);
	AT("%z %Z", 0, "-0545 -0545");
}

/* strftime, with a format the compiler cannot check, for malformed ones. */
static size_t unchecked(char *s, size_t n, const char *format, const struct tm *tm)
{
	return strftime(s, n, format, tm);
}

static void edges(void)
{
	char small[5];
	struct tm tm;
	time_t t = 0;

	CHECK(setenv("TZ", "UTC0", 1) == 0);
	CHECK(gmtime_r(&t, &tm) == &tm);

	/* Too small: 0, with as much as fits, terminated. */
	memset(small, 'x', sizeof small);
	CHECK(strftime(small, sizeof small, "%Y-%m", &tm) == 0);
	CHECK(strcmp(small, "1970") == 0);
	CHECK(strftime(small, sizeof small, "%Y", &tm) == 4);
	CHECK(strcmp(small, "1970") == 0);
	CHECK(strftime(small, sizeof small, "12345", &tm) == 0);
	CHECK(strftime(small, 0, "%Y", &tm) == 0);
	CHECK(unchecked(buffer, sizeof buffer, "", &tm) == 0);
	CHECK(buffer[0] == 0);

	/* An unknown conversion ends the result, as in musl. */
	CHECK(unchecked(buffer, sizeof buffer, "a%Qb", &tm) == 0);
	CHECK(unchecked(buffer, sizeof buffer, "a%", &tm) == 0);

	/* strftime_l ignores its locale. */
	CHECK(strftime_l(buffer, sizeof buffer, "%a %d", &tm, (locale_t)0) == 6);
	CHECK(strcmp(buffer, "Thu 01") == 0);

	/* musl: a width applies to %C %F %G %Y only; glibc would pad %A. */
	checkStrftime("%10A|%5d", &tm, "Thursday|01");

	/* musl: out-of-range names print "-". */
	struct tm odd = tm;
	odd.tm_wday = 9;
	odd.tm_mon = -1;
	checkStrftime("%a|%B", &odd, "-|-");

	/* No offset or zone when tm_isdst is negative. */
	odd = tm;
	odd.tm_isdst = -1;
	checkStrftime("[%z%Z]", &odd, "[]");

	/* musl: %Z prints a tm_zone the library did not hand out as nothing. */
	odd = tm;
	odd.tm_zone = "XYZ";
	checkStrftime("[%Z]", &odd, "[]");
	odd.tm_zone = 0;
	checkStrftime("[%Z]", &odd, "[]");

	/* %s honours tm_gmtoff. */
	odd = tm;
	odd.tm_gmtoff = 3600;
	checkStrftime("%s", &odd, "-3600");
}

int main(void)
{
	libc_test();
	tables();
	edges();
	return t_status;
}
