/*
 * timerfd as foot's event loop uses it: a one-shot timer that makes its
 * descriptor readable in an epoll set and reads as one expiry, then EAGAIN;
 * a periodic timer that counts every period that passed; the time left read
 * back; disarming; an absolute time already past, which fires at once; and
 * the errors.
 *
 * The periods are long enough that a loaded host still sees the counts it
 * checks, and the checks ask for at least what must have happened rather than
 * for exact numbers where scheduling could add one.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

#define MS (1000 * 1000)

static void nap(long nanos)
{
	struct timespec t = { nanos / 1000000000, nanos % 1000000000 };
	while (nanosleep(&t, &t) == -1 && errno == EINTR)
		;
}

int main(void)
{
	struct itimerspec set = { { 0, 0 }, { 0, 0 } }, got, old;
	struct epoll_event event = { .events = EPOLLIN }, ready;
	struct timespec now;
	uint64_t count = 0;
	char small[4];
	int fd, epoll;

	errno = 0;
	CHECK(timerfd_create(CLOCK_MONOTONIC, 0x1234) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(timerfd_create(12345, 0) == -1 && errno == EINVAL);

	fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
	CHECK(fd >= 0);
	/* Disarmed: nothing to read, and nothing left. */
	errno = 0;
	CHECK(read(fd, &count, sizeof count) == -1 && errno == EAGAIN);
	CHECK(timerfd_gettime(fd, &got) == 0);
	CHECK(got.it_value.tv_sec == 0 && got.it_value.tv_nsec == 0);

	/* A one-shot timer, seen by epoll_wait. */
	epoll = epoll_create1(EPOLL_CLOEXEC);
	CHECK(epoll >= 0 && epoll_ctl(epoll, EPOLL_CTL_ADD, fd, &event) == 0);
	set.it_value.tv_nsec = 50 * MS;
	CHECK(timerfd_settime(fd, 0, &set, &old) == 0);
	CHECK(old.it_value.tv_sec == 0 && old.it_value.tv_nsec == 0);
	CHECK(timerfd_gettime(fd, &got) == 0);
	CHECK(got.it_value.tv_sec == 0 && got.it_value.tv_nsec > 0 && got.it_value.tv_nsec <= 50 * MS);
	CHECK(got.it_interval.tv_sec == 0 && got.it_interval.tv_nsec == 0);
	CHECK(epoll_wait(epoll, &ready, 1, 5000) == 1 && ready.events & EPOLLIN);
	errno = 0;
	CHECK(read(fd, small, sizeof small) == -1 && errno == EINVAL);
	CHECK(read(fd, &count, sizeof count) == sizeof count && count == 1);
	errno = 0;
	CHECK(read(fd, &count, sizeof count) == -1 && errno == EAGAIN);
	CHECK(timerfd_gettime(fd, &got) == 0);
	CHECK(got.it_value.tv_sec == 0 && got.it_value.tv_nsec == 0);

	/* A periodic one counts every period that passed. */
	set.it_value.tv_nsec = 10 * MS;
	set.it_interval.tv_nsec = 10 * MS;
	CHECK(timerfd_settime(fd, 0, &set, 0) == 0);
	nap(65 * MS);
	CHECK(read(fd, &count, sizeof count) == sizeof count && count >= 5);
	CHECK(timerfd_gettime(fd, &got) == 0);
	CHECK(got.it_interval.tv_nsec == 10 * MS && got.it_value.tv_nsec <= 10 * MS);

	/* Disarmed, with what it was handed back. */
	set.it_value.tv_nsec = 0;
	set.it_interval.tv_nsec = 0;
	CHECK(timerfd_settime(fd, 0, &set, &old) == 0);
	CHECK(old.it_interval.tv_nsec == 10 * MS);
	nap(25 * MS);
	CHECK(poll(&(struct pollfd){ .fd = fd, .events = POLLIN }, 1, 0) == 0);

	/* An absolute time already past fires at once. */
	CHECK(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
	set.it_value = now;
	set.it_value.tv_sec -= 1;
	if (set.it_value.tv_sec < 0) {
		set.it_value.tv_sec = 0;
		set.it_value.tv_nsec = 1;
	}
	CHECK(timerfd_settime(fd, TFD_TIMER_ABSTIME, &set, 0) == 0);
	CHECK(poll(&(struct pollfd){ .fd = fd, .events = POLLIN }, 1, 5000) == 1);
	CHECK(read(fd, &count, sizeof count) == sizeof count && count == 1);

	/* The errors. */
	set.it_value.tv_nsec = 1000000000;
	errno = 0;
	CHECK(timerfd_settime(fd, 0, &set, 0) == -1 && errno == EINVAL);
	set.it_value.tv_nsec = 0;
	errno = 0;
	CHECK(timerfd_settime(fd, 0x40, &set, 0) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(timerfd_settime(epoll, 0, &set, 0) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(timerfd_gettime(-1, &got) == -1 && errno == EBADF);

	close(epoll);
	close(fd);
	return t_status;
}
