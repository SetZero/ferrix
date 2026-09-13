/*
 * Zone files that fail validation, read through TZDIR from the directory
 * tests/c_time.rs fills: each must give UTC under the name UTC, as in musl,
 * without a crash. An intact copy between them shows TZDIR is read, and that
 * every change of TZ reads the zone again.
 */

#define _GNU_SOURCE
#include <stdlib.h>
#include <time.h>
#include "check.h"
#include "report.h"

static void expect(const char *tz, long gmtoff, const char *zone)
{
	int before = t_status;
	time_t t = 1720000000;
	struct tm tm;

	t_status = 0;
	CHECK(setenv("TZ", tz, 1) == 0);
	tzset();
	CHECK(strcmp(tzname[0], gmtoff ? "CET" : "UTC") == 0);
	CHECK(daylight == (gmtoff != 0));
	CHECK(localtime_r(&t, &tm) == &tm);
	CHECK(tm.tm_gmtoff == gmtoff);
	CHECK(strcmp(tm.tm_zone, zone) == 0);
	if (t_status) {
		say("  with TZ=");
		say(tz);
		say("\n");
	}
	t_status |= before;
}

int main(void)
{
	static const char *const corrupt[] = {
		"Empty", "Short", "HeaderOnly", "Truncated", "NoNewline",
		"BadMagic", "BadVersion", "HugeCount", "BadType", "Unsorted",
		"BadAbbrev", "BadOffset", "BadFooter", "TrailingData", "Dir",
		"Missing",
	};
	char path[4200];
	const char *dir = getenv("TZDIR");

	CHECK(dir != 0);
	if (!dir)
		return t_status;
	for (size_t i = 0; i < sizeof corrupt / sizeof corrupt[0]; i++) {
		expect("Valid", 7200, "CEST");
		expect(corrupt[i], 0, "UTC");
		/* The same file by path. */
		strcpy(path, dir);
		strcat(path, "/");
		strcat(path, corrupt[i]);
		expect(path, 0, "UTC");
	}
	expect(":Valid", 7200, "CEST");
	return t_status;
}
