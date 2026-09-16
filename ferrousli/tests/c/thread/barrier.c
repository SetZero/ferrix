/*
 * Barriers and spin locks: rounds of a private barrier each return
 * PTHREAD_BARRIER_SERIAL_THREAD to exactly one thread; a process-shared
 * barrier in shared memory lets a parent and child meet; a spin lock guards a
 * counter across threads; and the attribute and count errors.
 *
 * Nothing depends on timing: every thread of a round waits at the barrier
 * until the round is full.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

enum { THREADS = 5, ROUNDS = 40, SPINNERS = 4, SPINS = 20000 };

static pthread_barrier_t barrier;
static pthread_spinlock_t spin;
static long counter;

static void *meet(void *arg)
{
	(void)arg;
	long serial = 0;
	for (int i = 0; i < ROUNDS; i++) {
		int ret = pthread_barrier_wait(&barrier);
		if (ret == PTHREAD_BARRIER_SERIAL_THREAD)
			serial++;
		else if (ret != 0)
			return (void *)-1L;
	}
	return (void *)serial;
}

static void *count(void *arg)
{
	(void)arg;
	for (int i = 0; i < SPINS; i++) {
		pthread_spin_lock(&spin);
		counter++;
		pthread_spin_unlock(&spin);
	}
	return 0;
}

int main(void)
{
	pthread_barrierattr_t attr;
	int shared = -1;
	CHECK(pthread_barrierattr_init(&attr) == 0);
	CHECK(pthread_barrierattr_getpshared(&attr, &shared) == 0);
	CHECK(shared == PTHREAD_PROCESS_PRIVATE);
	CHECK(pthread_barrierattr_setpshared(&attr, 2) == EINVAL);
	CHECK(pthread_barrier_init(&barrier, 0, 0) == EINVAL);

	CHECK(pthread_barrier_init(&barrier, &attr, 1) == 0);
	CHECK(pthread_barrier_wait(&barrier) == PTHREAD_BARRIER_SERIAL_THREAD);
	CHECK(pthread_barrier_destroy(&barrier) == 0);

	CHECK(pthread_barrier_init(&barrier, 0, THREADS) == 0);
	pthread_t threads[THREADS];
	for (int i = 0; i < THREADS; i++)
		CHECK(pthread_create(&threads[i], 0, meet, 0) == 0);
	long serial = 0;
	for (int i = 0; i < THREADS; i++) {
		void *ret;
		CHECK(pthread_join(threads[i], &ret) == 0);
		CHECK((long)ret >= 0);
		serial += (long)ret;
	}
	CHECK(serial == ROUNDS);
	CHECK(pthread_barrier_destroy(&barrier) == 0);

	CHECK(pthread_barrierattr_setpshared(&attr, PTHREAD_PROCESS_SHARED) == 0);
	CHECK(pthread_barrierattr_getpshared(&attr, &shared) == 0);
	CHECK(shared == PTHREAD_PROCESS_SHARED);
	pthread_barrier_t *pb = mmap(0, sizeof *pb, PROT_READ | PROT_WRITE,
				     MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	CHECK(pb != MAP_FAILED);
	CHECK(pthread_barrier_init(pb, &attr, 2) == 0);
	pid_t child = fork();
	CHECK(child >= 0);
	if (child == 0) {
		int ret = pthread_barrier_wait(pb);
		_exit(ret == 0 || ret == PTHREAD_BARRIER_SERIAL_THREAD ? ret == 0 : 99);
	}
	int ret = pthread_barrier_wait(pb);
	CHECK(ret == 0 || ret == PTHREAD_BARRIER_SERIAL_THREAD);
	int status;
	CHECK(waitpid(child, &status, 0) == child);
	CHECK(WIFEXITED(status));
	/* The child exits 1 if it got 0, and 0 if it was the serial thread:
	 * exactly one of the two is. */
	CHECK(WEXITSTATUS(status) == (ret == PTHREAD_BARRIER_SERIAL_THREAD));
	CHECK(pthread_barrier_destroy(pb) == 0);
	CHECK(pthread_barrierattr_destroy(&attr) == 0);

	CHECK(pthread_spin_init(&spin, PTHREAD_PROCESS_PRIVATE) == 0);
	CHECK(pthread_spin_trylock(&spin) == 0);
	CHECK(pthread_spin_trylock(&spin) == EBUSY);
	CHECK(pthread_spin_unlock(&spin) == 0);
	pthread_t spinners[SPINNERS];
	for (int i = 0; i < SPINNERS; i++)
		CHECK(pthread_create(&spinners[i], 0, count, 0) == 0);
	for (int i = 0; i < SPINNERS; i++)
		CHECK(pthread_join(spinners[i], 0) == 0);
	CHECK(counter == (long)SPINNERS * SPINS);
	CHECK(pthread_spin_destroy(&spin) == 0);

	return t_status;
}
