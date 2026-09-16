/*
 * C11 threads: creating, joining and detaching threads and their int results,
 * thrd_exit, thrd_current and thrd_equal, sleeping and yielding; call_once
 * run once across threads; plain, recursive and timed mutexes with their
 * busy and timeout results; condition variables signalled, broadcast and
 * timing out; and thread-specific storage with its destructor.
 *
 * Nothing depends on timing but the timeouts, which only need to expire.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <threads.h>
#include <time.h>

#include "check.h"

enum { WORKERS = 8, ROUNDS = 10000 };

static mtx_t lock;
static cnd_t cond;
static long counter;
static int ready, woken, go;
static once_flag once = ONCE_FLAG_INIT;
static int once_runs;
static tss_t key;
static int destroyed;

static struct timespec soon(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	ts.tv_nsec += 20 * 1000 * 1000;
	if (ts.tv_nsec >= 1000000000) {
		ts.tv_sec++;
		ts.tv_nsec -= 1000000000;
	}
	return ts;
}

static void init_once(void)
{
	once_runs++;
}

static int count(void *arg)
{
	call_once(&once, init_once);
	for (int i = 0; i < ROUNDS; i++) {
		if (mtx_lock(&lock) != thrd_success)
			return -1;
		counter++;
		mtx_unlock(&lock);
	}
	return (int)(long)arg;
}

static int ends_early(void *arg)
{
	(void)arg;
	thrd_exit(42);
}

static void destroy(void *value)
{
	if (value == &destroyed)
		destroyed++;
}

static int uses_storage(void *arg)
{
	(void)arg;
	if (tss_get(key) != 0 || tss_set(key, &destroyed) != thrd_success)
		return 1;
	return tss_get(key) == &destroyed ? 0 : 1;
}

static int holds_lock(void *arg)
{
	mtx_t *m = arg;
	mtx_lock(m);
	mtx_lock(&lock);
	ready = 1;
	cnd_broadcast(&cond);
	while (!go)
		cnd_wait(&cond, &lock);
	mtx_unlock(&lock);
	mtx_unlock(m);
	return 0;
}

static int waits(void *arg)
{
	(void)arg;
	mtx_lock(&lock);
	ready++;
	cnd_broadcast(&cond);
	while (!go)
		cnd_wait(&cond, &lock);
	woken++;
	mtx_unlock(&lock);
	return 0;
}

int main(void)
{
	CHECK(mtx_init(&lock, mtx_plain) == thrd_success);
	CHECK(cnd_init(&cond) == thrd_success);

	/* Threads, results and call_once. */
	thrd_t threads[WORKERS];
	for (int i = 0; i < WORKERS; i++)
		CHECK(thrd_create(&threads[i], count, (void *)(long)(i + 1)) == thrd_success);
	for (int i = 0; i < WORKERS; i++) {
		int result = 0;
		CHECK(thrd_join(threads[i], &result) == thrd_success);
		CHECK(result == i + 1);
	}
	CHECK(counter == (long)WORKERS * ROUNDS);
	CHECK(once_runs == 1);

	thrd_t t;
	int result = 0;
	CHECK(thrd_create(&t, ends_early, 0) == thrd_success);
	CHECK(thrd_join(t, &result) == thrd_success && result == 42);
	CHECK(thrd_create(&t, count, 0) == thrd_success);
	CHECK(thrd_detach(t) == thrd_success);
	CHECK(thrd_equal(thrd_current(), thrd_current()));
	CHECK((thrd_equal)(thrd_current(), thrd_current()) != 0);
	thrd_yield();
	struct timespec nap = { 0, 1000 * 1000 };
	CHECK(thrd_sleep(&nap, 0) == 0);

	/* Mutexes. */
	mtx_t recursive, timed;
	CHECK(mtx_init(&recursive, mtx_plain | mtx_recursive) == thrd_success);
	CHECK(mtx_lock(&recursive) == thrd_success);
	CHECK(mtx_trylock(&recursive) == thrd_success);
	CHECK(mtx_unlock(&recursive) == thrd_success);
	CHECK(mtx_unlock(&recursive) == thrd_success);
	mtx_destroy(&recursive);
	CHECK(mtx_init(&timed, mtx_timed) == thrd_success);
	CHECK(mtx_init(&recursive, 99) == thrd_error);

	ready = go = 0;
	CHECK(thrd_create(&t, holds_lock, &timed) == thrd_success);
	mtx_lock(&lock);
	while (!ready)
		cnd_wait(&cond, &lock);
	mtx_unlock(&lock);
	CHECK(mtx_trylock(&timed) == thrd_busy);
	struct timespec at = soon();
	CHECK(mtx_timedlock(&timed, &at) == thrd_timedout);
	mtx_lock(&lock);
	go = 1;
	cnd_broadcast(&cond);
	mtx_unlock(&lock);
	CHECK(thrd_join(t, 0) == thrd_success);
	at = soon();
	CHECK(mtx_timedlock(&timed, &at) == thrd_success);
	CHECK(mtx_unlock(&timed) == thrd_success);
	mtx_destroy(&timed);

	/* Condition variables. */
	mtx_lock(&lock);
	at = soon();
	CHECK(cnd_timedwait(&cond, &lock, &at) == thrd_timedout);
	mtx_unlock(&lock);
	ready = go = woken = 0;
	for (int i = 0; i < 4; i++)
		CHECK(thrd_create(&threads[i], waits, 0) == thrd_success);
	mtx_lock(&lock);
	while (ready < 4)
		cnd_wait(&cond, &lock);
	go = 1;
	CHECK(cnd_signal(&cond) == thrd_success);
	CHECK(cnd_broadcast(&cond) == thrd_success);
	mtx_unlock(&lock);
	for (int i = 0; i < 4; i++)
		CHECK(thrd_join(threads[i], 0) == thrd_success);
	CHECK(woken == 4);

	/* Thread-specific storage. */
	CHECK(tss_create(&key, destroy) == thrd_success);
	CHECK(tss_set(key, &result) == thrd_success && tss_get(key) == &result);
	CHECK(thrd_create(&t, uses_storage, 0) == thrd_success);
	CHECK(thrd_join(t, &result) == thrd_success && result == 0);
	CHECK(destroyed == 1);
	CHECK(tss_get(key) == &result);
	tss_delete(key);

	cnd_destroy(&cond);
	mtx_destroy(&lock);
	return t_status;
}
