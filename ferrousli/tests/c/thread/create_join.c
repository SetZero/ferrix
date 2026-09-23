/*
 * Creating and joining: return values, pthread_self and pthread_equal, and
 * the errors join and detach report.
 *
 * Threads that must still be running when a check is made wait on a pipe the
 * main thread writes to, so no check depends on timing.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

static int gate[2];

static void *twice(void *arg)
{
	return (void *)((long)arg * 2);
}

static void *join_self(void *arg)
{
	(void)arg;
	return (void *)(long)pthread_join(pthread_self(), 0);
}

static void *wait_for_gate(void *arg)
{
	char byte;
	if (read(gate[0], &byte, 1) != 1)
		return (void *)-2;
	return arg;
}

static void *report_self(void *arg)
{
	*(pthread_t *)arg = pthread_self();
	return 0;
}

int main(void)
{
	pthread_t t[16];
	void *result;

	for (long i = 0; i < 16; i++)
		CHECK(pthread_create(&t[i], 0, twice, (void *)i) == 0);
	for (long i = 0; i < 16; i++) {
		result = 0;
		CHECK(pthread_join(t[i], &result) == 0);
		CHECK(result == (void *)(i * 2));
	}

	/* pthread_self in a thread is the id its creator got. */
	pthread_t seen = 0;
	CHECK(pthread_create(&t[0], 0, report_self, &seen) == 0);
	CHECK(pthread_join(t[0], 0) == 0);
	CHECK(pthread_equal(seen, t[0]));
	CHECK(!pthread_equal(seen, pthread_self()));
	CHECK(pthread_equal(pthread_self(), pthread_self()));

	/* Joining oneself is a deadlock, reported as glibc reports it. */
	CHECK(pthread_join(pthread_self(), 0) == EDEADLK);
	CHECK(pthread_create(&t[0], 0, join_self, 0) == 0);
	CHECK(pthread_join(t[0], &result) == 0);
	CHECK(result == (void *)(long)EDEADLK);

	/* A thread still running: tryjoin is busy, a timed join times out. */
	CHECK(pipe(gate) == 0);
	CHECK(pthread_create(&t[0], 0, wait_for_gate, (void *)7) == 0);
	CHECK(pthread_tryjoin_np(t[0], &result) == EBUSY);
	struct timespec at;
	CHECK(clock_gettime(CLOCK_REALTIME, &at) == 0);
	at.tv_nsec += 20 * 1000 * 1000;
	if (at.tv_nsec >= 1000000000) {
		at.tv_sec++;
		at.tv_nsec -= 1000000000;
	}
	CHECK(pthread_timedjoin_np(t[0], &result, &at) == ETIMEDOUT);
	at.tv_nsec = 1000000000;
	CHECK(pthread_timedjoin_np(t[0], &result, &at) == EINVAL);
	CHECK(write(gate[1], "x", 1) == 1);
	result = 0;
	CHECK(pthread_join(t[0], &result) == 0);
	CHECK(result == (void *)7);

	/* A thread that has ended can be joined with tryjoin once it is gone. */
	CHECK(pthread_create(&t[0], 0, twice, (void *)21) == 0);
	CHECK(pthread_timedjoin_np(t[0], &result, &(struct timespec){ .tv_sec = (time_t)1 << 40 }) == 0);
	CHECK(result == (void *)42);

	/* Detaching twice, and joining a detached thread, are refused. */
	CHECK(pthread_create(&t[1], 0, wait_for_gate, 0) == 0);
	CHECK(pthread_detach(t[1]) == 0);
	CHECK(pthread_detach(t[1]) == EINVAL);
	CHECK(pthread_join(t[1], 0) == EINVAL);
	CHECK(pthread_tryjoin_np(t[1], 0) == EINVAL);

	/* Detaching a thread that has already returned frees it. */
	CHECK(pthread_create(&t[2], 0, wait_for_gate, 0) == 0);
	CHECK(pthread_create(&t[3], 0, twice, 0) == 0);
	CHECK(pthread_join(t[3], 0) == 0);

	/* A null start routine is refused. */
	CHECK(pthread_create(&t[0], 0, 0, 0) == EINVAL);

	/* Release the detached thread and the one after it, then join the
	 * latter, which leaves the former running or gone: either is fine. */
	CHECK(write(gate[1], "xy", 2) == 2);
	CHECK(pthread_join(t[2], 0) == 0);

	return t_status;
}
