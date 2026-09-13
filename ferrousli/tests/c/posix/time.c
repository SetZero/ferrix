/*
 * Time: the clocks, sleeping, gettimeofday against time, and alarm.
 *
 * The first check is adapted from libc-test's src/functional/clock_gettime.c
 * (MIT).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

static long long nanoseconds(const struct timespec *ts)
{
	return ts->tv_sec * 1000000000LL + ts->tv_nsec;
}

static long long monotonic(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return nanoseconds(&ts);
}

int main(void)
{
	struct timespec a, b, req;
	struct timeval tv;
	long long start;
	time_t before, stored, after;

	errno = 0;
	CHECK(clock_gettime(CLOCK_REALTIME, &a) == 0 && errno == 0);
	CHECK(a.tv_nsec >= 0 && a.tv_nsec < 1000000000);

	/* CLOCK_MONOTONIC never goes backwards. */
	CHECK(clock_gettime(CLOCK_MONOTONIC, &a) == 0);
	for (int i = 0; i < 10000; i++) {
		CHECK(clock_gettime(CLOCK_MONOTONIC, &b) == 0);
		CHECK(nanoseconds(&b) >= nanoseconds(&a));
		a = b;
	}
	errno = 0;
	CHECK(clock_gettime(12345, &a) == -1 && errno == EINVAL);
	CHECK(clock_getres(CLOCK_MONOTONIC, &b) == 0 && b.tv_sec == 0 && b.tv_nsec > 0);
	CHECK(clock_getres(CLOCK_REALTIME, 0) == 0);
	errno = 0;
	CHECK(clock_getres(12345, &b) == -1 && errno == EINVAL);
	/* The monotonic clock cannot be set, whatever the privilege. */
	errno = 0;
	CHECK(clock_settime(CLOCK_MONOTONIC, &a) == -1 && errno == EINVAL);

	/* nanosleep sleeps at least as long as asked. */
	req.tv_sec = 0;
	req.tv_nsec = 20000000;
	start = monotonic();
	CHECK(nanosleep(&req, &b) == 0);
	CHECK(monotonic() - start >= 20000000);
	req.tv_nsec = 1000000000;
	errno = 0;
	CHECK(nanosleep(&req, 0) == -1 && errno == EINVAL);
	req.tv_sec = -1;
	req.tv_nsec = 0;
	errno = 0;
	CHECK(nanosleep(&req, 0) == -1 && errno == EINVAL);

	/* clock_nanosleep returns the error, and leaves errno alone. */
	req.tv_sec = 0;
	req.tv_nsec = -1;
	errno = 0;
	CHECK(clock_nanosleep(CLOCK_MONOTONIC, 0, &req, 0) == EINVAL && errno == 0);
	req.tv_nsec = 1000;
	CHECK(clock_nanosleep(CLOCK_THREAD_CPUTIME_ID, 0, &req, 0) == EINVAL && errno == 0);
	CHECK(clock_nanosleep(12345, 0, &req, 0) == EINVAL && errno == 0);
	CHECK(clock_nanosleep(CLOCK_MONOTONIC, 0, &req, 0) == 0);

	/* An absolute sleep lasts until the time; one in the past returns. */
	clock_gettime(CLOCK_MONOTONIC, &req);
	req.tv_nsec += 10000000;
	if (req.tv_nsec >= 1000000000) {
		req.tv_nsec -= 1000000000;
		req.tv_sec++;
	}
	CHECK(clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &req, 0) == 0);
	CHECK(monotonic() >= nanoseconds(&req));
	req.tv_sec -= 1;
	start = monotonic();
	CHECK(clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &req, 0) == 0);
	CHECK(monotonic() - start < 500000000);

	/* usleep and sleep. */
	start = monotonic();
	CHECK(usleep(2000) == 0);
	CHECK(monotonic() - start >= 2000000);
	CHECK(sleep(0) == 0);

	/* gettimeofday and CLOCK_REALTIME agree with time. */
	before = time(0);
	CHECK(gettimeofday(&tv, 0) == 0);
	CHECK(clock_gettime(CLOCK_REALTIME, &a) == 0);
	after = time(&stored);
	CHECK(stored == after);
	CHECK(tv.tv_sec >= before && tv.tv_sec <= after);
	CHECK(a.tv_sec >= tv.tv_sec && a.tv_sec <= after);
	CHECK(tv.tv_usec >= 0 && tv.tv_usec < 1000000);
	CHECK(before > 1600000000);
	CHECK(gettimeofday(0, 0) == 0);

	/* alarm reports what was left, and zero cancels. */
	CHECK(alarm(100) == 0);
	CHECK(alarm(50) == 100);
	CHECK(alarm(0) == 50);
	CHECK(alarm(0) == 0);

	return t_status;
}
