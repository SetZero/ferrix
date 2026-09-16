/*
 * pthread_cancel and pthread_testcancel: a thread blocked in read, in
 * nanosleep and in pthread_cond_wait is cancelled there, with its cleanup
 * handlers run; one with cancellation disabled carries on until it enables it;
 * an asynchronous one is cancelled wherever it is; a thread cancels itself;
 * a thread that never enables cancellation finishes its work; and a cancel
 * sent before a thread reaches its first cancellation point acts there.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

static _Atomic int cleaned;
static _Atomic int started;

static void cleanup(void *arg)
{
	cleaned += (int)(long)arg;
}

static void wait_started(void)
{
	while (!started) {
		struct timespec ts = { 0, 1000000 };
		nanosleep(&ts, 0);
	}
	/* Let the thread reach its blocking call. */
	struct timespec ts = { 0, 50000000 };
	nanosleep(&ts, 0);
}

static int pipe_fds[2];

static void *blocked_in_read(void *arg)
{
	(void)arg;
	char byte;
	pthread_cleanup_push(cleanup, (void *)1);
	started = 1;
	read(pipe_fds[0], &byte, 1);
	pthread_cleanup_pop(0);
	return (void *)7;
}

static void *blocked_in_sleep(void *arg)
{
	(void)arg;
	pthread_cleanup_push(cleanup, (void *)10);
	started = 1;
	struct timespec ts = { 60, 0 };
	nanosleep(&ts, 0);
	pthread_cleanup_pop(0);
	return (void *)7;
}

static _Atomic int after_disabled;

static void *disabled_then_enabled(void *arg)
{
	(void)arg;
	int old;
	CHECK(pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &old) == 0);
	CHECK(old == PTHREAD_CANCEL_ENABLE);
	started = 1;
	/* A cancellation point with cancellation disabled does not act on the
	 * request. The signal that carries it still interrupts a sleep, as it
	 * does under musl, since Linux restarts no nanosleep after a handler. */
	struct timespec ts = { 0, 200000000 };
	errno = 0;
	CHECK(nanosleep(&ts, 0) == 0 || errno == EINTR);
	after_disabled = 1;
	CHECK(pthread_setcancelstate(PTHREAD_CANCEL_ENABLE, 0) == 0);
	pthread_testcancel();
	after_disabled = 2;
	return 0;
}

static volatile unsigned long spins;

static void *asynchronous(void *arg)
{
	(void)arg;
	CHECK(pthread_setcanceltype(PTHREAD_CANCEL_ASYNCHRONOUS, 0) == 0);
	started = 1;
	for (;;)
		spins++;
	return 0;
}

static void *cancels_itself(void *arg)
{
	(void)arg;
	CHECK(pthread_cancel(pthread_self()) == 0);
	/* Deferred: nothing happens until a cancellation point. */
	after_disabled = 3;
	pthread_testcancel();
	after_disabled = 4;
	return 0;
}

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;

static void unlock(void *arg)
{
	/* The mutex is held again when a cancelled wait runs its handlers. */
	CHECK(pthread_mutex_unlock(arg) == 0);
	cleaned += 100;
}

static void *waiting_on_condition(void *arg)
{
	(void)arg;
	CHECK(pthread_mutex_lock(&lock) == 0);
	pthread_cleanup_push(unlock, &lock);
	started = 1;
	for (;;)
		pthread_cond_wait(&cond, &lock);
	pthread_cleanup_pop(1);
	return 0;
}

static void *reads_what_is_there(void *arg)
{
	(void)arg;
	char byte = 0;
	int state;
	CHECK(pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &state) == 0);
	started = 1;
	CHECK(read(pipe_fds[0], &byte, 1) == 1 && byte == 'x');
	return (void *)(long)byte;
}

static void *cancelled(pthread_t t)
{
	void *result = 0;
	CHECK(pthread_join(t, &result) == 0);
	return result;
}

int main(void)
{
	pthread_t t;

	CHECK(pipe(pipe_fds) == 0);
	started = 0;
	CHECK(pthread_create(&t, 0, blocked_in_read, 0) == 0);
	wait_started();
	CHECK(pthread_cancel(t) == 0);
	CHECK(cancelled(t) == PTHREAD_CANCELED);
	CHECK(cleaned == 1);

	started = 0;
	CHECK(pthread_create(&t, 0, blocked_in_sleep, 0) == 0);
	wait_started();
	CHECK(pthread_cancel(t) == 0);
	CHECK(cancelled(t) == PTHREAD_CANCELED);
	CHECK(cleaned == 11);

	started = 0;
	CHECK(pthread_create(&t, 0, disabled_then_enabled, 0) == 0);
	while (!started)
		sched_yield();
	CHECK(pthread_cancel(t) == 0);
	CHECK(cancelled(t) == PTHREAD_CANCELED);
	CHECK(after_disabled == 1);

	started = 0;
	CHECK(pthread_create(&t, 0, asynchronous, 0) == 0);
	wait_started();
	CHECK(pthread_cancel(t) == 0);
	CHECK(cancelled(t) == PTHREAD_CANCELED);
	CHECK(spins > 0);

	CHECK(pthread_create(&t, 0, cancels_itself, 0) == 0);
	CHECK(cancelled(t) == PTHREAD_CANCELED);
	CHECK(after_disabled == 3);

	started = 0;
	CHECK(pthread_create(&t, 0, waiting_on_condition, 0) == 0);
	wait_started();
	CHECK(pthread_mutex_lock(&lock) == 0);
	CHECK(pthread_mutex_unlock(&lock) == 0);
	CHECK(pthread_cancel(t) == 0);
	CHECK(cancelled(t) == PTHREAD_CANCELED);
	CHECK(cleaned == 111);
	CHECK(pthread_mutex_trylock(&lock) == 0);
	CHECK(pthread_mutex_unlock(&lock) == 0);

	started = 0;
	CHECK(pthread_create(&t, 0, reads_what_is_there, 0) == 0);
	CHECK(write(pipe_fds[1], "x", 1) == 1);
	wait_started();
	CHECK(pthread_cancel(t) == 0);
	CHECK(cancelled(t) == (void *)(long)'x');

	/* Cancelled before it reaches nanosleep: the flag is read first there. */
	CHECK(pthread_create(&t, 0, blocked_in_sleep, 0) == 0);
	CHECK(pthread_cancel(t) == 0);
	CHECK(cancelled(t) == PTHREAD_CANCELED);
	return t_status;
}
