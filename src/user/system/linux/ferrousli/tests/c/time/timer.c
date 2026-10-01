/*
 * POSIX timers: one that notifies nobody, armed, read back and deleted; one
 * that sends a blocked SIGUSR1, armed, read back and disarmed, then fired
 * with a value taken by sigtimedwait, then made periodic, with the missed
 * periods timer_getoverrun counts; the errors for a deleted timer and a bad
 * clock; and SIGEV_THREAD, which this library refuses with ENOTSUP rather
 * than never calling the function. __cxa_at_quick_exit, which glibc exports
 * beside the timers Claude Code imports, registers as at_quick_exit does.
 *
 * The periods are long enough that a loaded host still sees what is checked,
 * and the checks ask for at least what must have happened.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdlib.h>
#include <time.h>

#include "check.h"

#define MS (1000 * 1000)

int __cxa_at_quick_exit(void (*)(void), void *);

static void nothing(void)
{
}

static void on_thread(union sigval value)
{
	(void)value;
}

int main(void)
{
	struct itimerspec set = { { 0, 0 }, { 0, 0 } }, got, old;
	struct sigevent event = { 0 };
	struct timespec wait = { 5, 0 };
	siginfo_t info;
	sigset_t usr1;
	timer_t timer;
	int marker = 42;

	/* No notification: armed, read back, deleted. Linux 7.0 still reports
	 * time left on a disarmed SIGEV_NONE timer, so the disarm is checked on
	 * the next one. */
	event.sigev_notify = SIGEV_NONE;
	CHECK(timer_create(CLOCK_MONOTONIC, &event, &timer) == 0);
	CHECK(timer_gettime(timer, &got) == 0);
	CHECK(got.it_value.tv_sec == 0 && got.it_value.tv_nsec == 0);
	set.it_value.tv_sec = 60;
	CHECK(timer_settime(timer, 0, &set, &old) == 0);
	CHECK(old.it_value.tv_sec == 0 && old.it_value.tv_nsec == 0);
	CHECK(timer_gettime(timer, &got) == 0);
	CHECK(got.it_value.tv_sec > 50 && got.it_value.tv_sec <= 60);
	CHECK(timer_delete(timer) == 0);
	errno = 0;
	CHECK(timer_gettime(timer, &got) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(timer_delete(timer) == -1 && errno == EINVAL);

	/* A blocked signal: armed, read back, disarmed. */
	sigemptyset(&usr1);
	sigaddset(&usr1, SIGUSR1);
	CHECK(sigprocmask(SIG_BLOCK, &usr1, 0) == 0);
	event.sigev_notify = SIGEV_SIGNAL;
	event.sigev_signo = SIGUSR1;
	event.sigev_value.sival_ptr = &marker;
	CHECK(timer_create(CLOCK_MONOTONIC, &event, &timer) == 0);
	CHECK(timer_settime(timer, 0, &set, 0) == 0);
	set.it_value.tv_sec = 0;
	CHECK(timer_settime(timer, 0, &set, &old) == 0);
	CHECK(old.it_value.tv_sec > 50 && old.it_value.tv_sec <= 60);
	CHECK(timer_gettime(timer, &got) == 0 && got.it_value.tv_sec == 0 && got.it_value.tv_nsec == 0);

	/* Fired, with the value. */
	set.it_value.tv_nsec = 20 * MS;
	CHECK(timer_settime(timer, 0, &set, 0) == 0);
	CHECK(sigtimedwait(&usr1, &info, &wait) == SIGUSR1);
	CHECK(info.si_code == SI_TIMER && info.si_value.sival_ptr == &marker);

	/* Periodic, with the signal left pending: the periods after the first
	 * are overruns. */
	set.it_value.tv_nsec = 10 * MS;
	set.it_interval.tv_nsec = 10 * MS;
	CHECK(timer_settime(timer, 0, &set, 0) == 0);
	{
		struct timespec nap = { 0, 200 * MS };
		while (nanosleep(&nap, &nap) == -1 && errno == EINTR)
			;
	}
	CHECK(sigtimedwait(&usr1, &info, &wait) == SIGUSR1);
	CHECK(info.si_overrun >= 5);
	CHECK(timer_getoverrun(timer) >= 5);
	CHECK(timer_delete(timer) == 0);

	/* A clock that is not one. */
	event.sigev_notify = SIGEV_NONE;
	errno = 0;
	CHECK(timer_create(12345, &event, &timer) == -1 && errno == EINVAL);

	/* SIGEV_THREAD, refused. */
	event.sigev_notify = SIGEV_THREAD;
	event.sigev_notify_function = on_thread;
	errno = 0;
	CHECK(timer_create(CLOCK_MONOTONIC, &event, &timer) == -1 && errno == ENOTSUP);

	CHECK(__cxa_at_quick_exit(nothing, 0) == 0);
	CHECK(__cxa_at_quick_exit(0, 0) == -1);
	return t_status;
}
