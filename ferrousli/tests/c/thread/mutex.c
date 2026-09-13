/*
 * Mutexes: a shared counter under each type, and each type's errors.
 *
 * Threads that must hold a mutex while another thread tries it signal on a
 * pipe once they hold it, and wait on another pipe before letting go, so no
 * check depends on timing.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

enum { THREADS = 8, ROUNDS = 20000 };

static pthread_mutex_t m;
static long counter;
static int recursive;

static void *count(void *arg)
{
	(void)arg;
	for (int i = 0; i < ROUNDS; i++) {
		if (pthread_mutex_lock(&m))
			return (void *)1;
		if (recursive && pthread_mutex_lock(&m))
			return (void *)1;
		long seen = counter;
		counter = seen + 1;
		if (recursive && pthread_mutex_unlock(&m))
			return (void *)1;
		if (pthread_mutex_unlock(&m))
			return (void *)1;
	}
	return 0;
}

static void counter_under(int type)
{
	pthread_mutexattr_t attr;
	CHECK(pthread_mutexattr_init(&attr) == 0);
	CHECK(pthread_mutexattr_settype(&attr, type) == 0);
	CHECK(pthread_mutex_init(&m, &attr) == 0);
	CHECK(pthread_mutexattr_destroy(&attr) == 0);
	counter = 0;
	recursive = type == PTHREAD_MUTEX_RECURSIVE;
	pthread_t t[THREADS];
	for (int i = 0; i < THREADS; i++)
		CHECK(pthread_create(&t[i], 0, count, 0) == 0);
	for (int i = 0; i < THREADS; i++) {
		void *result = (void *)1;
		CHECK(pthread_join(t[i], &result) == 0);
		CHECK(result == 0);
	}
	CHECK(counter == (long)THREADS * ROUNDS);
	CHECK(pthread_mutex_destroy(&m) == 0);
}

/* What another thread gets from each call on a mutex the main thread holds. */
static struct {
	int unlock;
	int trylock;
	int timedlock;
} other;

static void *try_from_other_thread(void *arg)
{
	(void)arg;
	struct timespec at;
	clock_gettime(CLOCK_REALTIME, &at);
	at.tv_nsec += 20 * 1000 * 1000;
	if (at.tv_nsec >= 1000000000) {
		at.tv_sec++;
		at.tv_nsec -= 1000000000;
	}
	other.unlock = pthread_mutex_unlock(&m);
	other.trylock = pthread_mutex_trylock(&m);
	other.timedlock = pthread_mutex_timedlock(&m, &at);
	return 0;
}

static void from_other_thread(void)
{
	pthread_t t;
	CHECK(pthread_create(&t, 0, try_from_other_thread, 0) == 0);
	CHECK(pthread_join(t, 0) == 0);
}

static pthread_mutex_t with_type(int type)
{
	pthread_mutexattr_t attr;
	pthread_mutex_t mutex;
	CHECK(pthread_mutexattr_init(&attr) == 0);
	CHECK(pthread_mutexattr_settype(&attr, type) == 0);
	CHECK(pthread_mutex_init(&mutex, &attr) == 0);
	return mutex;
}

int main(void)
{
	counter_under(PTHREAD_MUTEX_NORMAL);
	counter_under(PTHREAD_MUTEX_RECURSIVE);
	counter_under(PTHREAD_MUTEX_ERRORCHECK);

	/* Attributes. */
	pthread_mutexattr_t attr;
	int value = -1;
	CHECK(pthread_mutexattr_init(&attr) == 0);
	CHECK(pthread_mutexattr_gettype(&attr, &value) == 0 && value == PTHREAD_MUTEX_DEFAULT);
	CHECK(pthread_mutexattr_settype(&attr, 3) == EINVAL);
	CHECK(pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_ERRORCHECK) == 0);
	CHECK(pthread_mutexattr_gettype(&attr, &value) == 0 && value == PTHREAD_MUTEX_ERRORCHECK);
	CHECK(pthread_mutexattr_setrobust(&attr, 2) == EINVAL);
	CHECK(pthread_mutexattr_getrobust(&attr, &value) == 0 && value == PTHREAD_MUTEX_STALLED);
	CHECK(pthread_mutexattr_setpshared(&attr, 2) == EINVAL);
	CHECK(pthread_mutexattr_getpshared(&attr, &value) == 0 && value == PTHREAD_PROCESS_PRIVATE);
	CHECK(pthread_mutexattr_setprotocol(&attr, PTHREAD_PRIO_PROTECT) == ENOTSUP);
	CHECK(pthread_mutexattr_setprotocol(&attr, 7) == EINVAL);
	CHECK(pthread_mutexattr_getprotocol(&attr, &value) == 0 && value == PTHREAD_PRIO_NONE);
	CHECK(pthread_mutexattr_destroy(&attr) == 0);

	/* Normal: busy while held; anyone may unlock it, as musl allows. */
	m = with_type(PTHREAD_MUTEX_NORMAL);
	CHECK(pthread_mutex_lock(&m) == 0);
	CHECK(pthread_mutex_trylock(&m) == EBUSY);
	CHECK(pthread_mutex_unlock(&m) == 0);
	CHECK(pthread_mutex_trylock(&m) == 0);
	CHECK(pthread_mutex_unlock(&m) == 0);

	/* Error-checking: relocking and foreign unlocks are refused. */
	m = with_type(PTHREAD_MUTEX_ERRORCHECK);
	CHECK(pthread_mutex_unlock(&m) == EPERM);
	CHECK(pthread_mutex_lock(&m) == 0);
	CHECK(pthread_mutex_lock(&m) == EDEADLK);
	CHECK(pthread_mutex_trylock(&m) == EBUSY);
	struct timespec at = { 0, 0 };
	CHECK(pthread_mutex_timedlock(&m, &at) == EDEADLK);
	from_other_thread();
	CHECK(other.unlock == EPERM);
	CHECK(other.trylock == EBUSY);
	CHECK(other.timedlock == ETIMEDOUT);
	CHECK(pthread_mutex_unlock(&m) == 0);
	CHECK(pthread_mutex_unlock(&m) == EPERM);

	/* Recursive: holds count, and only the owner releases them. */
	m = with_type(PTHREAD_MUTEX_RECURSIVE);
	CHECK(pthread_mutex_lock(&m) == 0);
	CHECK(pthread_mutex_lock(&m) == 0);
	CHECK(pthread_mutex_trylock(&m) == 0);
	from_other_thread();
	CHECK(other.unlock == EPERM);
	CHECK(other.trylock == EBUSY);
	CHECK(other.timedlock == ETIMEDOUT);
	CHECK(pthread_mutex_unlock(&m) == 0);
	CHECK(pthread_mutex_unlock(&m) == 0);
	CHECK(pthread_mutex_unlock(&m) == 0);
	CHECK(pthread_mutex_unlock(&m) == EPERM);

	/* A timed lock with a bad time is refused once it would wait. */
	m = with_type(PTHREAD_MUTEX_NORMAL);
	CHECK(pthread_mutex_lock(&m) == 0);
	at.tv_nsec = 1000000000;
	CHECK(pthread_mutex_timedlock(&m, &at) == EINVAL);
	at.tv_nsec = 0;
	CHECK(pthread_mutex_timedlock(&m, &at) == ETIMEDOUT);
	CHECK(pthread_mutex_unlock(&m) == 0);
	CHECK(pthread_mutex_timedlock(&m, &at) == 0);
	CHECK(pthread_mutex_unlock(&m) == 0);

	/* Priority ceilings are not supported. */
	CHECK(pthread_mutex_getprioceiling(&m, &value) == EINVAL);
	return t_status;
}
