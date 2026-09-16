/*
 * Scheduling policy, priority and affinity, for the process's calls in
 * sched.h and a thread's in pthread.h: the priority ranges, reading and
 * setting SCHED_OTHER's policy and priority, the errors for a bad priority,
 * the round-robin interval, each thread's affinity and the processor it runs
 * on, a thread's CPU-time clock, and the concurrency hint.
 *
 * Every change asked for is one an unprivileged process may make.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <time.h>

#include "check.h"

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t done = PTHREAD_COND_INITIALIZER;
static int finish;

static void *idle(void *arg)
{
	(void)arg;
	/* Burn a little CPU time for the clock to count. */
	volatile unsigned long spin = 0;
	for (unsigned long i = 0; i < 2000000; i++)
		spin += i;
	pthread_mutex_lock(&lock);
	while (!finish)
		pthread_cond_wait(&done, &lock);
	pthread_mutex_unlock(&lock);
	return 0;
}

int main(void)
{
	CHECK(sched_get_priority_max(SCHED_FIFO) == 99);
	CHECK(sched_get_priority_min(SCHED_FIFO) == 1);
	CHECK(sched_get_priority_max(SCHED_OTHER) == 0);
	CHECK(sched_get_priority_min(-5) == -1 && errno == EINVAL);

	struct sched_param param = { .sched_priority = 7 };
	CHECK(sched_getscheduler(0) == SCHED_OTHER);
	CHECK(sched_getparam(0, &param) == 0 && param.sched_priority == 0);
	CHECK(sched_setparam(0, &param) == 0);
	CHECK(sched_setscheduler(0, SCHED_OTHER, &param) == 0);
	param.sched_priority = 5;
	CHECK(sched_setscheduler(0, SCHED_OTHER, &param) == -1 && errno == EINVAL);

	struct timespec ts = { -1, -1 };
	CHECK(sched_rr_get_interval(0, &ts) == 0);
	CHECK(ts.tv_sec >= 0 && ts.tv_nsec >= 0);

	pthread_t self = pthread_self();
	int policy = -1;
	param.sched_priority = 7;
	CHECK(pthread_getschedparam(self, &policy, &param) == 0);
	CHECK(policy == SCHED_OTHER && param.sched_priority == 0);
	CHECK(pthread_setschedparam(self, SCHED_OTHER, &param) == 0);
	CHECK(pthread_setschedprio(self, 0) == 0);
	CHECK(pthread_setschedprio(self, 5) == EINVAL);
	param.sched_priority = 500;
	CHECK(pthread_setschedparam(self, SCHED_FIFO, &param) == EINVAL);

	cpu_set_t set;
	CPU_ZERO(&set);
	CHECK(sched_getaffinity(0, sizeof set, &set) == 0);
	CHECK(CPU_COUNT(&set) >= 1);
	int cpu = sched_getcpu();
	CHECK(cpu >= 0 && CPU_ISSET(cpu, &set));

	pthread_t thread;
	CHECK(pthread_create(&thread, 0, idle, 0) == 0);
	cpu_set_t other;
	CPU_ZERO(&other);
	CHECK(pthread_getaffinity_np(thread, sizeof other, &other) == 0);
	CHECK(CPU_EQUAL(&set, &other));
	CHECK(pthread_setaffinity_np(thread, sizeof other, &other) == 0);
	CHECK(pthread_getschedparam(thread, &policy, &param) == 0);
	CHECK(policy == SCHED_OTHER);

	clockid_t clock;
	CHECK(pthread_getcpuclockid(thread, &clock) == 0);
	struct timespec used = { -1, -1 };
	CHECK(clock_gettime(clock, &used) == 0);
	CHECK(used.tv_sec >= 0 && used.tv_nsec >= 0);
	CHECK(pthread_getcpuclockid(self, &clock) == 0);
	CHECK(clock_gettime(clock, &used) == 0);
	CHECK(used.tv_sec > 0 || used.tv_nsec > 0);

	pthread_mutex_lock(&lock);
	finish = 1;
	pthread_cond_signal(&done);
	pthread_mutex_unlock(&lock);
	CHECK(pthread_join(thread, 0) == 0);

	CHECK(pthread_getconcurrency() == 0);
	CHECK(pthread_setconcurrency(-1) == EINVAL);
	CHECK(pthread_setconcurrency(4) == 0);
	CHECK(pthread_getconcurrency() == 4);

	return t_status;
}
