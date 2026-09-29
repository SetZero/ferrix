/*
 * strptime: musl's conversions, the %F, %s and %z it adds, failures, where
 * parsing stops, and round trips through strftime. Run with TZ=UTC0.
 *
 * The first part is adapted from libc-test's src/functional/strptime.c (MIT).
 */

#define _GNU_SOURCE
#include <stddef.h>
#include <stdlib.h>
#include <time.h>
#include "check.h"
#include "report.h"

static void check_at(int line, const char *s, const char *format, const struct tm *expected)
{
	struct tm tm = { 0 };
	const char *ret = strptime(s, format, &tm);

	if (!ret || *ret != '\0' || tm.tm_sec != expected->tm_sec ||
	    tm.tm_min != expected->tm_min || tm.tm_hour != expected->tm_hour ||
	    tm.tm_mday != expected->tm_mday || tm.tm_mon != expected->tm_mon ||
	    tm.tm_year != expected->tm_year) {
		say("strptime.c:");
		say_number(line);
		say(": \"");
		say(format);
		say("\" did not parse \"");
		say(s);
		say("\" as expected\n");
		t_status = 1;
	}
}

#define checkStrptime(s, f, e) check_at(__LINE__, s, f, e)

static void checkStrptimeTz(const char *s, int h, int m)
{
	struct tm tm = { 0 };
	const char *ret = strptime(s, "%z", &tm);
	CHECK(ret && *ret == '\0');
	CHECK(tm.tm_gmtoff == h * 3600 + m * 60);
}

static struct tm tm1 = { .tm_sec = 8, .tm_min = 57, .tm_hour = 20 };
static struct tm tm2 = { .tm_mday = 25, .tm_mon = 8 - 1, .tm_year = 1991 - 1900 };
static struct tm tm3 = { .tm_mday = 21, .tm_mon = 10 - 1, .tm_year = 2015 - 1900 };
static struct tm tm4 = { .tm_mday = 10, .tm_mon = 7 - 1, .tm_year = 1856 - 1900 };

static void libc_test(void)
{
	checkStrptime("20:57:08", "%H:%M:%S", &tm1);
	checkStrptime("20:57:8", "%R:%S", &tm1);
	checkStrptime("20:57:08", "%T", &tm1);

	checkStrptime("20:57:08", "%H : %M  :  %S", &tm1);
	checkStrptime("20 57  08", "%H %M %S", &tm1);
	checkStrptime("20%57%08", "%H %% %M%%%S", &tm1);
	checkStrptime("foo20bar57qux08      ", "foo %Hbar %M qux%S ", &tm1);

	checkStrptime("1991-08-25", "%Y-%m-%d", &tm2);
	checkStrptime("25.08.91", "%d.%m.%y", &tm2);
	checkStrptime("08/25/91", "%D", &tm2);
	checkStrptime("21.10.15", "%d.%m.%y", &tm3);
	checkStrptime("10.7.56 in 18th", "%d.%m.%y in %C th", &tm4);

	checkStrptime("1856-07-10", "%F", &tm4);
	checkStrptime("683078400", "%s", &tm2);
	checkStrptimeTz("+0200", 2, 0);
	checkStrptimeTz("-0530", -5, -30);
	checkStrptimeTz("-06", -6, 0);
}

static void conversions(void)
{
	struct tm tm = { 0 };
	const char *s;

	/* Names, in any case, full names before abbreviations. */
	s = "wednesday, 3 SEPTEMBER 2025 pm 07";
	CHECK(strptime(s, "%A, %e %B %Y %p %I", &tm) == s + strlen(s));
	CHECK(tm.tm_wday == 3 && tm.tm_mday == 3 && tm.tm_mon == 8);
	CHECK(tm.tm_year == 125);
	/* %p before %I sets nothing lasting: %I reads 7 afterwards. */
	CHECK(tm.tm_hour == 7);
	s = "Thu Feb  1 23:05:09 1973 rest";
	tm = (struct tm){ 0 };
	CHECK(strptime(s, "%c", &tm) == s + 24);
	CHECK(tm.tm_wday == 4 && tm.tm_mon == 1 && tm.tm_mday == 1);
	CHECK(tm.tm_hour == 23 && tm.tm_min == 5 && tm.tm_sec == 9 && tm.tm_year == 73);

	/* %p after the hour. */
	tm = (struct tm){ 0 };
	CHECK(strptime("12:30 AM", "%I:%M %p", &tm) != 0);
	CHECK(tm.tm_hour == 0);
	CHECK(strptime("12:30 pm", "%I:%M %p", &tm) != 0);
	CHECK(tm.tm_hour == 12);
	CHECK(strptime("01:30 PM", "%r", &tm) == 0);
	CHECK(strptime("01:30:00 PM", "%r", &tm) != 0);
	CHECK(tm.tm_hour == 13);

	/* %j, %w, %U and %W; %U and %W are read and discarded, as in musl. */
	tm = (struct tm){ 0 };
	CHECK(strptime("366 6 53 00", "%j %w %U %W", &tm) != 0);
	CHECK(tm.tm_yday == 365 && tm.tm_wday == 6);

	/* %y alone: 69 to 99 are 1900s, 0 to 68 the 2000s. */
	CHECK(strptime("68", "%y", &tm) && tm.tm_year == 168);
	CHECK(strptime("69", "%y", &tm) && tm.tm_year == 69);
	CHECK(strptime("20 07", "%C %y", &tm) && tm.tm_year == 107);
	CHECK(strptime("-0044", "%Y", &tm) && tm.tm_year == -1944);
	CHECK(strptime("123456", "%Y", &tm) && tm.tm_year == 1234 - 1900);
	CHECK(strptime("123456", "%6Y", &tm) && tm.tm_year == 123456 - 1900);

	/* White space, %n and %t. */
	s = " \t\n12";
	CHECK(strptime(s, "%n%H", &tm) == s + 5);
	CHECK(strptime("12\t:\n 30", "%H%t:%n%M", &tm) != 0);
	/* A number does not skip white space before it. */
	CHECK(strptime("12: 30", "%H:%M", &tm) == 0);
	CHECK(tm.tm_min == 30);

	/* E and O are ignored. */
	CHECK(strptime("2024 07", "%EY %Om", &tm) != 0);
	CHECK(tm.tm_year == 124 && tm.tm_mon == 6);

	/* Parsing stops where the format ends. */
	s = "2024-07-03T11";
	CHECK(strptime(s, "%Y-%m-%d", &tm) == s + 10);

	/* %s is local time. */
	CHECK(setenv("TZ", "CET-1CEST,M3.5.0,M10.5.0/3", 1) == 0);
	tm = (struct tm){ 0 };
	CHECK(strptime("1720000000", "%s", &tm) != 0);
	CHECK(tm.tm_hour == 11 && tm.tm_isdst == 1 && tm.tm_gmtoff == 7200);
	CHECK(strptime("-1", "%s", &tm) != 0);
	CHECK(tm.tm_year == 70 && tm.tm_hour == 0 && tm.tm_min == 59);
	CHECK(setenv("TZ", "UTC0", 1) == 0);

	checkStrptimeTz("+05:45", 5, 45);
	checkStrptimeTz("Z", 0, 0);
}

static void failures(void)
{
	struct tm tm = { 0 };
	static const char *const bad[][2] = {
		{"24", "%H"}, {"13", "%m"}, {"0", "%m"}, {"0", "%d"}, {"32", "%d"},
		{"367", "%j"}, {"60", "%M"}, {"62", "%S"}, {"0", "%I"}, {"13", "%I"},
		{"7", "%w"}, {"x", "%Y"}, {"", "%Y"}, {"-", "%Y"}, {"Jux", "%b"},
		{"Fr", "%A"}, {"XM", "%p"}, {"20", "%H:%M"}, {"5", "%Q"},
		{"5", "%"}, {"a", "%%"}, {"", "%%"}, {"2024", "x%Y"}, {"+25", "%z"},
		{"0200", "%z"}, {"+0260", "%z"}, {"99999999999999999999", "%s"},
		{"", "a"},
	};

	for (size_t i = 0; i < sizeof bad / sizeof bad[0]; i++) {
		if (strptime(bad[i][0], bad[i][1], &tm) != 0) {
			say("strptime.c: \"");
			say(bad[i][1]);
			say("\" parsed \"");
			say(bad[i][0]);
			say("\"\n");
			t_status = 1;
		}
	}
	/* An empty format matches nothing, successfully. */
	CHECK(strptime("abc", "", &tm) != 0);
}

static void round_trips(void)
{
	static const long long times[] = {
		0, 951782400, 1720000000, 1735689599, 2147483648LL, -1, 68169600,
	};
	static const char *const formats[] = {
		"%Y-%m-%d %H:%M:%S", "%c", "%D %r", "%F %T", "%a %e %b %Y %R:%S",
		"%A %d %B %C%y %I:%M:%S %p", "%x %X", "%Y %j %T",
	};

	for (size_t i = 0; i < sizeof times / sizeof times[0]; i++) {
		for (size_t j = 0; j < sizeof formats / sizeof formats[0]; j++) {
			time_t t = times[i];
			struct tm tm, back = { 0 };
			char text[100];
			int before = t_status;

			t_status = 0;
			gmtime_r(&t, &tm);
			CHECK(strftime(text, sizeof text, formats[j], &tm) > 0);
			const char *end = strptime(text, formats[j], &back);
			CHECK(end && *end == 0);
			CHECK(back.tm_year == tm.tm_year);
			CHECK(back.tm_hour == tm.tm_hour && back.tm_min == tm.tm_min);
			CHECK(back.tm_sec == tm.tm_sec);
			if (formats[j][1] == 'Y' && formats[j][3] == '%' && formats[j][4] == 'j')
				CHECK(back.tm_yday == tm.tm_yday);
			else
				CHECK(back.tm_mon == tm.tm_mon && back.tm_mday == tm.tm_mday);
			if (t_status) {
				say("  round trip of \"");
				say(text);
				say("\" through \"");
				say(formats[j]);
				say("\"\n");
			}
			t_status |= before;
		}
	}
}

int main(void)
{
	libc_test();
	conversions();
	failures();
	round_trips();
	return t_status;
}
