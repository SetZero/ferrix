/*
 * The first threaded program for Ferrix to run as its first process, with
 * `cargo xtask test-shell --init <program>`. It also runs in ferrousli's
 * own tests.
 *
 * It ignores its arguments, and needs no files, no environment and no /proc.
 * Each step is announced on standard output before it starts and confirmed
 * when it passes, so a kernel developer can see exactly where it stops. It
 * ends with "pthread: all ok" and status 0, or "pthread: FAILED ..." and
 * status 1.
 *
 * Nothing depends on how threads are scheduled: every wait is on a join, a
 * mutex or a condition variable with its predicate checked, never a sleep.
 */

#include <errno.h>
#include <pthread.h>
#include <string.h>
#include <unistd.h>

static void say(const char *line)
{
	write(1, "pthread: ", 9);
	write(1, line, strlen(line));
	write(1, "\n", 1);
}

static int fail(const char *what)
{
	write(1, "pthread: FAILED ", 16);
	write(1, what, strlen(what));
	write(1, "\n", 1);
	return 1;
}

/* create/join: each thread returns a value computed from its argument. */
static void *twice_plus_one(void *arg)
{
	return (void *)((long)arg * 2 + 1);
}

/* mutex counter: an increment in two steps, which a broken mutex loses. */
enum { WORKERS = 4, ROUNDS = 20000 };
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static long counter;

static void *count(void *arg)
{
	(void)arg;
	for (int i = 0; i < ROUNDS; i++) {
		if (pthread_mutex_lock(&lock))
			return (void *)1;
		long seen = counter;
		counter = seen + 1;
		if (pthread_mutex_unlock(&lock))
			return (void *)1;
	}
	return 0;
}

/* condition variable: main sets stage 1, the thread answers with stage 2. */
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
static int stage;

static void *answer(void *arg)
{
	(void)arg;
	pthread_mutex_lock(&lock);
	while (stage != 1)
		pthread_cond_wait(&cond, &lock);
	stage = 2;
	pthread_cond_broadcast(&cond);
	pthread_mutex_unlock(&lock);
	return 0;
}

/* thread-local storage: each thread starts with the image's values. */
static _Thread_local int initialised = 42;
static _Thread_local long zeroed;

static void *check_tls(void *arg)
{
	long id = (long)arg;
	if (initialised != 42 || zeroed != 0)
		return (void *)1;
	initialised = (int)id;
	zeroed = id + 100;
	if (initialised != id || zeroed != id + 100)
		return (void *)1;
	return 0;
}

/* errno: a thread's errno starts at 0 and is its own. */
static void *check_errno(void *arg)
{
	(void)arg;
	if (errno != 0)
		return (void *)1;
	if (close(-1) != -1 || errno != EBADF)
		return (void *)1;
	return 0;
}

int main(int argc, char **argv)
{
	(void)argc;
	(void)argv;
	pthread_t t[WORKERS];
	void *result;

	say("create/join");
	for (long i = 0; i < WORKERS; i++)
		if (pthread_create(&t[i], 0, twice_plus_one, (void *)i))
			return fail("pthread_create");
	for (long i = 0; i < WORKERS; i++) {
		if (pthread_join(t[i], &result))
			return fail("pthread_join");
		if (result != (void *)(i * 2 + 1))
			return fail("a joined thread's return value");
	}
	say("create/join ok");

	say("mutex counter");
	for (int i = 0; i < WORKERS; i++)
		if (pthread_create(&t[i], 0, count, 0))
			return fail("pthread_create");
	for (int i = 0; i < WORKERS; i++)
		if (pthread_join(t[i], &result) || result)
			return fail("a counting thread");
	if (counter != (long)WORKERS * ROUNDS)
		return fail("the counter lost increments");
	say("mutex counter ok");

	say("condition variable");
	if (pthread_mutex_lock(&lock))
		return fail("pthread_mutex_lock");
	if (pthread_create(&t[0], 0, answer, 0))
		return fail("pthread_create");
	stage = 1;
	pthread_cond_broadcast(&cond);
	while (stage != 2)
		if (pthread_cond_wait(&cond, &lock))
			return fail("pthread_cond_wait");
	pthread_mutex_unlock(&lock);
	if (pthread_join(t[0], 0))
		return fail("pthread_join");
	say("condition variable ok");

	say("thread-local storage");
	initialised = 7;
	zeroed = 8;
	for (long i = 0; i < WORKERS; i++)
		if (pthread_create(&t[i], 0, check_tls, (void *)(i + 1)))
			return fail("pthread_create");
	for (int i = 0; i < WORKERS; i++)
		if (pthread_join(t[i], &result) || result)
			return fail("a thread's _Thread_local values");
	if (initialised != 7 || zeroed != 8)
		return fail("the main thread's _Thread_local values");
	say("thread-local storage ok");

	say("errno");
	errno = 12345;
	for (int i = 0; i < WORKERS; i++)
		if (pthread_create(&t[i], 0, check_errno, 0))
			return fail("pthread_create");
	for (int i = 0; i < WORKERS; i++)
		if (pthread_join(t[i], &result) || result)
			return fail("a thread's errno");
	if (errno != 12345)
		return fail("the main thread's errno");
	say("errno ok");

	say("all ok");
	return 0;
}
