/*
 * Condition variables: a producer and consumer queue, timed waits that time
 * out on both clocks, broadcast waking every waiter, signal waking waiters
 * one by one, and the errors.
 *
 * Adapted in part from libc-test's functional/pthread_cond.c and
 * regression/pthread_condattr_setclock.c (MIT). Every wait checks its
 * predicate, so nothing depends on timing.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <time.h>

#include "check.h"

enum { PRODUCERS = 3, CONSUMERS = 3, ITEMS = 2000, SLOTS = 4 };

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t not_empty = PTHREAD_COND_INITIALIZER;
static pthread_cond_t not_full = PTHREAD_COND_INITIALIZER;
static long queue[SLOTS];
static int head, used, producers_left = PRODUCERS;

static void *produce(void *arg)
{
	long base = (long)arg * ITEMS;
	for (long i = 1; i <= ITEMS; i++) {
		pthread_mutex_lock(&lock);
		while (used == SLOTS)
			pthread_cond_wait(&not_full, &lock);
		queue[(head + used++) % SLOTS] = base + i;
		pthread_cond_signal(&not_empty);
		pthread_mutex_unlock(&lock);
	}
	pthread_mutex_lock(&lock);
	producers_left--;
	pthread_cond_broadcast(&not_empty);
	pthread_mutex_unlock(&lock);
	return 0;
}

static void *consume(void *arg)
{
	long sum = 0;
	(void)arg;
	pthread_mutex_lock(&lock);
	for (;;) {
		while (used == 0 && producers_left)
			pthread_cond_wait(&not_empty, &lock);
		if (used == 0)
			break;
		sum += queue[head];
		head = (head + 1) % SLOTS;
		used--;
		pthread_cond_signal(&not_full);
	}
	pthread_mutex_unlock(&lock);
	return (void *)sum;
}

static pthread_cond_t go_cond = PTHREAD_COND_INITIALIZER;
static pthread_cond_t ready_cond = PTHREAD_COND_INITIALIZER;
static int waiting, go, woken;

static void *wait_for_go(void *arg)
{
	(void)arg;
	pthread_mutex_lock(&lock);
	waiting++;
	pthread_cond_signal(&ready_cond);
	while (!go)
		pthread_cond_wait(&go_cond, &lock);
	woken++;
	go--;
	pthread_mutex_unlock(&lock);
	return 0;
}

static struct timespec in_ms(clockid_t clock, long ms)
{
	struct timespec at;
	clock_gettime(clock, &at);
	at.tv_nsec += ms * 1000 * 1000;
	while (at.tv_nsec >= 1000000000) {
		at.tv_sec++;
		at.tv_nsec -= 1000000000;
	}
	return at;
}

int main(void)
{
	/* Producers and consumers: every item is consumed exactly once. */
	pthread_t p[PRODUCERS], c[CONSUMERS];
	for (long i = 0; i < CONSUMERS; i++)
		CHECK(pthread_create(&c[i], 0, consume, 0) == 0);
	for (long i = 0; i < PRODUCERS; i++)
		CHECK(pthread_create(&p[i], 0, produce, (void *)i) == 0);
	for (int i = 0; i < PRODUCERS; i++)
		CHECK(pthread_join(p[i], 0) == 0);
	long total = 0;
	for (int i = 0; i < CONSUMERS; i++) {
		void *sum = 0;
		CHECK(pthread_join(c[i], &sum) == 0);
		total += (long)sum;
	}
	long want = 0;
	for (long i = 0; i < PRODUCERS; i++)
		for (long j = 1; j <= ITEMS; j++)
			want += i * ITEMS + j;
	CHECK(total == want);

	/* A timed wait times out holding the mutex again, on either clock. */
	pthread_mutexattr_t mattr;
	pthread_mutex_t checked;
	CHECK(pthread_mutexattr_init(&mattr) == 0);
	CHECK(pthread_mutexattr_settype(&mattr, PTHREAD_MUTEX_ERRORCHECK) == 0);
	CHECK(pthread_mutex_init(&checked, &mattr) == 0);
	pthread_cond_t cond;
	pthread_condattr_t cattr;
	clockid_t clock = -1;
	CHECK(pthread_condattr_init(&cattr) == 0);
	CHECK(pthread_condattr_getclock(&cattr, &clock) == 0 && clock == CLOCK_REALTIME);
	CHECK(pthread_condattr_setclock(&cattr, CLOCK_PROCESS_CPUTIME_ID) == EINVAL);
	CHECK(pthread_condattr_setclock(&cattr, CLOCK_THREAD_CPUTIME_ID) == EINVAL);
	CHECK(pthread_cond_init(&cond, &cattr) == 0);
	struct timespec at = in_ms(CLOCK_REALTIME, 10);
	CHECK(pthread_cond_timedwait(&cond, &checked, &at) == EPERM);
	CHECK(pthread_mutex_lock(&checked) == 0);
	CHECK(pthread_cond_timedwait(&cond, &checked, &at) == ETIMEDOUT);
	at.tv_nsec = 1000000000;
	CHECK(pthread_cond_timedwait(&cond, &checked, &at) == EINVAL);
	CHECK(pthread_mutex_unlock(&checked) == 0);
	CHECK(pthread_cond_destroy(&cond) == 0);

	CHECK(pthread_condattr_setclock(&cattr, CLOCK_MONOTONIC) == 0);
	CHECK(pthread_condattr_getclock(&cattr, &clock) == 0 && clock == CLOCK_MONOTONIC);
	CHECK(pthread_cond_init(&cond, &cattr) == 0);
	CHECK(pthread_mutex_lock(&checked) == 0);
	at = in_ms(CLOCK_MONOTONIC, 10);
	CHECK(pthread_cond_timedwait(&cond, &checked, &at) == ETIMEDOUT);
	CHECK(pthread_mutex_unlock(&checked) == 0);
	CHECK(pthread_cond_destroy(&cond) == 0);

	/* Process-shared variables work within a process too. */
	int shared = -1;
	CHECK(pthread_condattr_setpshared(&cattr, 2) == EINVAL);
	CHECK(pthread_condattr_setpshared(&cattr, PTHREAD_PROCESS_SHARED) == 0);
	CHECK(pthread_condattr_getpshared(&cattr, &shared) == 0 && shared == 1);
	CHECK(pthread_cond_init(&cond, &cattr) == 0);
	CHECK(pthread_mutex_lock(&checked) == 0);
	at = in_ms(CLOCK_MONOTONIC, 10);
	CHECK(pthread_cond_timedwait(&cond, &checked, &at) == ETIMEDOUT);
	CHECK(pthread_mutex_unlock(&checked) == 0);
	CHECK(pthread_cond_destroy(&cond) == 0);
	CHECK(pthread_condattr_destroy(&cattr) == 0);

	/* Broadcast wakes every waiter; signal wakes them one at a time. */
	enum { WAITERS = 6 };
	pthread_t w[WAITERS];
	for (int round = 0; round < 2; round++) {
		waiting = woken = go = 0;
		for (int i = 0; i < WAITERS; i++)
			CHECK(pthread_create(&w[i], 0, wait_for_go, 0) == 0);
		pthread_mutex_lock(&lock);
		while (waiting < WAITERS)
			pthread_cond_wait(&ready_cond, &lock);
		if (round == 0) {
			go = WAITERS;
			pthread_cond_broadcast(&go_cond);
		} else {
			for (int i = 0; i < WAITERS; i++) {
				go++;
				pthread_cond_signal(&go_cond);
			}
		}
		pthread_mutex_unlock(&lock);
		for (int i = 0; i < WAITERS; i++)
			CHECK(pthread_join(w[i], 0) == 0);
		CHECK(woken == WAITERS);
	}
	return t_status;
}
