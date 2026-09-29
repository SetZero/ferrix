/*
 * The process's CPU time: clock counts up while the program works, and
 * times fills in struct tms, or accepts a null one.
 */

#include <sys/times.h>
#include <time.h>
#include "check.h"

static volatile unsigned long spin;

int main(void)
{
	struct tms before = { -1, -1, -1, -1 }, after;
	clock_t start = clock();

	CHECK(start >= 0);
	CHECK(times(&before) != (clock_t)-1);
	CHECK(before.tms_utime >= 0 && before.tms_stime >= 0);
	CHECK(before.tms_cutime == 0 && before.tms_cstime == 0);

	/* Work until clock has seen at least 20 ms. */
	while (clock() - start < CLOCKS_PER_SEC / 50)
		for (int i = 0; i < 100000; i++)
			spin += i;
	CHECK(clock() > start);
	CHECK(times(&after) != (clock_t)-1);
	CHECK(after.tms_utime + after.tms_stime >= before.tms_utime + before.tms_stime);
	CHECK(times(0) != (clock_t)-1);
	return t_status;
}
