/*
 * Semaphores: an unnamed one counting items between producer and consumer
 * threads; try, timed and clock waits failing as they should; the value and
 * overflow limits; a process-shared unnamed semaphore in shared memory
 * across fork; and named semaphores, opened, reopened as the same mapping,
 * refused when they exist or do not, posted by a child, closed and unlinked.
 *
 * Nothing depends on timing but the timeouts, which only need to expire.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <semaphore.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

enum { PRODUCERS = 3, CONSUMERS = 3, ITEMS = 5000 };

static sem_t items;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static long produced, consumed;

static void *produce(void *arg)
{
	(void)arg;
	for (int i = 0; i < ITEMS; i++) {
		pthread_mutex_lock(&lock);
		produced++;
		pthread_mutex_unlock(&lock);
		if (sem_post(&items) != 0)
			return (void *)1;
	}
	return 0;
}

static void *consume(void *arg)
{
	(void)arg;
	for (int i = 0; i < ITEMS; i++) {
		if (sem_wait(&items) != 0)
			return (void *)1;
		pthread_mutex_lock(&lock);
		consumed++;
		pthread_mutex_unlock(&lock);
	}
	return 0;
}

static struct timespec soon(clockid_t clock)
{
	struct timespec ts;
	clock_gettime(clock, &ts);
	ts.tv_nsec += 20 * 1000 * 1000;
	if (ts.tv_nsec >= 1000000000) {
		ts.tv_sec++;
		ts.tv_nsec -= 1000000000;
	}
	return ts;
}

int main(void)
{
	/* Counting between threads. */
	CHECK(sem_init(&items, 0, 0) == 0);
	pthread_t producers[PRODUCERS], consumers[CONSUMERS];
	for (int i = 0; i < CONSUMERS; i++)
		CHECK(pthread_create(&consumers[i], 0, consume, 0) == 0);
	for (int i = 0; i < PRODUCERS; i++)
		CHECK(pthread_create(&producers[i], 0, produce, 0) == 0);
	for (int i = 0; i < PRODUCERS; i++) {
		void *ret;
		CHECK(pthread_join(producers[i], &ret) == 0 && ret == 0);
	}
	for (int i = 0; i < CONSUMERS; i++) {
		void *ret;
		CHECK(pthread_join(consumers[i], &ret) == 0 && ret == 0);
	}
	CHECK(produced == (long)PRODUCERS * ITEMS && consumed == produced);
	int value = -1;
	CHECK(sem_getvalue(&items, &value) == 0 && value == 0);

	/* Failing waits. */
	errno = 0;
	CHECK(sem_trywait(&items) == -1 && errno == EAGAIN);
	struct timespec at = soon(CLOCK_REALTIME);
	CHECK(sem_timedwait(&items, &at) == -1 && errno == ETIMEDOUT);
	at = soon(CLOCK_MONOTONIC);
	CHECK(sem_clockwait(&items, CLOCK_MONOTONIC, &at) == -1 && errno == ETIMEDOUT);
	at.tv_nsec = 1000000000;
	CHECK(sem_timedwait(&items, &at) == -1 && errno == EINVAL);
	CHECK(sem_post(&items) == 0);
	at = soon(CLOCK_REALTIME);
	CHECK(sem_timedwait(&items, &at) == 0);
	CHECK(sem_destroy(&items) == 0);

	/* Limits. */
	CHECK(sem_init(&items, 0, (unsigned)SEM_VALUE_MAX + 1) == -1 && errno == EINVAL);
	CHECK(sem_init(&items, 0, SEM_VALUE_MAX) == 0);
	CHECK(sem_post(&items) == -1 && errno == EOVERFLOW);
	CHECK(sem_getvalue(&items, &value) == 0 && value == SEM_VALUE_MAX);
	CHECK(sem_trywait(&items) == 0);
	CHECK(sem_getvalue(&items, &value) == 0 && value == SEM_VALUE_MAX - 1);

	/* An unnamed semaphore shared with a child. */
	sem_t *shared = mmap(0, sizeof *shared, PROT_READ | PROT_WRITE,
			     MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	CHECK(shared != MAP_FAILED);
	CHECK(sem_init(shared, 1, 0) == 0);
	pid_t child = fork();
	CHECK(child >= 0);
	if (child == 0)
		_exit(sem_post(shared) == 0 && sem_post(shared) == 0 ? 0 : 1);
	CHECK(sem_wait(shared) == 0 && sem_wait(shared) == 0);
	int status;
	CHECK(waitpid(child, &status, 0) == child);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);

	/* Named semaphores. */
	char name[64];
	snprintf(name, sizeof name, "/ferrousli-sem-%d", (int)getpid());
	sem_unlink(name);
	errno = 0;
	CHECK(sem_open(name, 0) == SEM_FAILED && errno == ENOENT);
	CHECK(sem_open("a/b", O_CREAT, 0600, 0) == SEM_FAILED && errno == EINVAL);
	sem_t *named = sem_open(name, O_CREAT | O_EXCL, 0600, 2);
	CHECK(named != SEM_FAILED);
	CHECK(sem_open(name, O_CREAT | O_EXCL, 0600, 0) == SEM_FAILED && errno == EEXIST);
	sem_t *again = sem_open(name, O_CREAT, 0600, 9);
	CHECK(again == named);
	CHECK(sem_getvalue(named, &value) == 0 && value == 2);
	CHECK(sem_close(again) == 0);
	CHECK(sem_trywait(named) == 0 && sem_trywait(named) == 0);

	child = fork();
	CHECK(child >= 0);
	if (child == 0) {
		sem_t *theirs = sem_open(name, 0);
		_exit(theirs != SEM_FAILED && sem_post(theirs) == 0 ? 0 : 1);
	}
	CHECK(sem_wait(named) == 0);
	CHECK(waitpid(child, &status, 0) == child);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);

	CHECK(sem_close(&items) == -1 && errno == EINVAL);
	CHECK(sem_unlink(name) == 0);
	CHECK(sem_unlink(name) == -1 && errno == ENOENT);
	CHECK(sem_close(named) == 0);

	return t_status;
}
