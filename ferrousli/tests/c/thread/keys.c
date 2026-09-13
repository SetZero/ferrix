/*
 * Thread-specific data: values are per thread, destructors run at exit, and
 * destructors that set values again run for up to
 * PTHREAD_DESTRUCTOR_ITERATIONS rounds.
 *
 * Adapted in part from libc-test's functional/pthread_tsd.c (MIT).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <pthread.h>

#include "check.h"

static pthread_key_t k1, k2, again, chain_a, chain_b;
static int again_calls, chain_a_calls, chain_b_calls;

static void dtor(void *p)
{
	*(int *)p = 1;
}

static void *start(void *arg)
{
	int *p = arg;
	if (pthread_getspecific(k1) || pthread_getspecific(k2))
		return arg;
	if (pthread_setspecific(k1, p) || pthread_setspecific(k2, p + 1))
		return arg;
	if (pthread_getspecific(k1) != p)
		return arg;
	return 0;
}

/* Sets its own value again every time: stopped by the iteration limit. */
static void set_again(void *p)
{
	again_calls++;
	pthread_setspecific(again, p);
}

/* Each destructor sets the other key once, so the chain needs rounds. */
static void set_b(void *p)
{
	chain_a_calls++;
	if (chain_a_calls == 1)
		pthread_setspecific(chain_b, p);
}

static void set_a(void *p)
{
	chain_b_calls++;
	if (chain_b_calls == 1)
		pthread_setspecific(chain_a, p);
}

static void *start_chains(void *arg)
{
	pthread_setspecific(again, arg);
	pthread_setspecific(chain_a, arg);
	return 0;
}

static void *key_exists(void *arg)
{
	return pthread_getspecific(*(pthread_key_t *)arg);
}

int main(void)
{
	pthread_t td;
	void *res;
	int foo[2], bar[2];

	/* From pthread_tsd.c. */
	CHECK(pthread_key_create(&k1, dtor) == 0);
	CHECK(pthread_key_create(&k2, dtor) == 0);
	foo[0] = foo[1] = 0;
	CHECK(pthread_setspecific(k1, bar) == 0);
	CHECK(pthread_setspecific(k2, bar + 1) == 0);
	CHECK(pthread_create(&td, 0, start, foo) == 0);
	CHECK(pthread_join(td, &res) == 0);
	CHECK(res == 0);
	CHECK(foo[0] == 1);
	CHECK(foo[1] == 1);
	CHECK(pthread_getspecific(k1) == bar);
	CHECK(pthread_getspecific(k2) == bar + 1);
	CHECK(pthread_setspecific(k1, 0) == 0);
	CHECK(pthread_setspecific(k2, 0) == 0);
	CHECK(pthread_key_delete(k1) == 0);
	CHECK(pthread_key_delete(k2) == 0);

	/* Destructors that set values run again, up to the limit. */
	CHECK(pthread_key_create(&again, set_again) == 0);
	CHECK(pthread_key_create(&chain_a, set_b) == 0);
	CHECK(pthread_key_create(&chain_b, set_a) == 0);
	CHECK(pthread_create(&td, 0, start_chains, foo) == 0);
	CHECK(pthread_join(td, 0) == 0);
	CHECK(again_calls == PTHREAD_DESTRUCTOR_ITERATIONS);
	CHECK(chain_a_calls == 2);
	CHECK(chain_b_calls == 1);

	/* A deleted key forgets values, and its destructor no longer runs. */
	CHECK(pthread_setspecific(again, foo) == 0);
	CHECK(pthread_key_delete(again) == 0);
	CHECK(pthread_key_delete(again) == EINVAL);
	CHECK(pthread_getspecific(again) == 0);

	/* Keys run out, and out-of-range keys are refused. */
	static pthread_key_t keys[PTHREAD_KEYS_MAX];
	int made = 0;
	while (made < PTHREAD_KEYS_MAX && pthread_key_create(&keys[made], 0) == 0)
		made++;
	pthread_key_t extra;
	CHECK(pthread_key_create(&extra, 0) == EAGAIN);
	CHECK(made == PTHREAD_KEYS_MAX - 2);
	for (int i = 0; i < made; i++)
		CHECK(pthread_key_delete(keys[i]) == 0);
	CHECK(pthread_setspecific(PTHREAD_KEYS_MAX, foo) == EINVAL);
	CHECK(pthread_getspecific(PTHREAD_KEYS_MAX) == 0);
	CHECK(pthread_key_delete(PTHREAD_KEYS_MAX) == EINVAL);

	/* A new key has no value in a new thread. */
	pthread_key_t fresh;
	CHECK(pthread_key_create(&fresh, 0) == 0);
	CHECK(pthread_setspecific(fresh, foo) == 0);
	CHECK(pthread_create(&td, 0, key_exists, &fresh) == 0);
	CHECK(pthread_join(td, &res) == 0);
	CHECK(res == 0);
	return t_status;
}
