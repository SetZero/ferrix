/*
 * epoll and eventfd as an event loop uses them (calloop over the polling
 * crate does): an eventfd in an epoll set, written by another thread to wake
 * a blocked epoll_wait; the counter and EFD_SEMAPHORE; level- and
 * edge-triggered readiness and EPOLLONESHOT over a pipe; epoll_pwait and
 * epoll_pwait2 timing out; FIONBIO; and the errors.
 *
 * Nothing depends on timing but the timeouts, which only need to expire.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

static int wake_fd;

static void *waker(void *arg)
{
	(void)arg;
	struct timespec nap = { 0, 20 * 1000 * 1000 };
	nanosleep(&nap, 0);
	return (void *)(long)eventfd_write(wake_fd, 5);
}

int main(void)
{
	/* The errors. */
	errno = 0;
	CHECK(epoll_create(0) == -1 && errno == EINVAL);
	CHECK(epoll_create1(-1) == -1 && errno == EINVAL);
	int ep = epoll_create(10);
	CHECK(ep >= 0);
	CHECK(fcntl(ep, F_GETFD) == 0);
	int ep2 = epoll_create1(EPOLL_CLOEXEC);
	CHECK(ep2 >= 0 && fcntl(ep2, F_GETFD) == FD_CLOEXEC);
	struct epoll_event ev = { .events = EPOLLIN, .data.u64 = 7 };
	CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, ep, &ev) == -1 && errno == EINVAL);
	CHECK(epoll_ctl(ep, EPOLL_CTL_DEL, 12345, 0) == -1 && errno == EBADF);
#if defined(__x86_64__)
	/* Packed on x86-64 only, as the kernel's is. */
	CHECK(sizeof(struct epoll_event) == 12);
#else
	CHECK(sizeof(struct epoll_event) == 16);
#endif

	/* An eventfd wakes a blocked wait from another thread. */
	wake_fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
	CHECK(wake_fd >= 0);
	ev.events = EPOLLIN;
	ev.data.u64 = 0x1122334455667788ULL;
	CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, wake_fd, &ev) == 0);
	CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, wake_fd, &ev) == -1 && errno == EEXIST);
	pthread_t t;
	CHECK(pthread_create(&t, 0, waker, 0) == 0);
	struct epoll_event out[4];
	CHECK(epoll_wait(ep, out, 4, -1) == 1);
	CHECK(out[0].events == EPOLLIN && out[0].data.u64 == 0x1122334455667788ULL);
	void *ret;
	CHECK(pthread_join(t, &ret) == 0 && ret == 0);
	eventfd_t value = 0;
	CHECK(eventfd_read(wake_fd, &value) == 0 && value == 5);
	CHECK(eventfd_read(wake_fd, &value) == -1 && errno == EAGAIN);
	CHECK(epoll_wait(ep, out, 4, 0) == 0);

	/* The counter adds up; EFD_SEMAPHORE hands it out one at a time. */
	CHECK(eventfd_write(wake_fd, 2) == 0 && eventfd_write(wake_fd, 3) == 0);
	CHECK(eventfd_read(wake_fd, &value) == 0 && value == 5);
	int sem = eventfd(2, EFD_SEMAPHORE | EFD_NONBLOCK);
	CHECK(sem >= 0);
	CHECK(eventfd_read(sem, &value) == 0 && value == 1);
	CHECK(eventfd_read(sem, &value) == 0 && value == 1);
	CHECK(eventfd_read(sem, &value) == -1 && errno == EAGAIN);
	CHECK(eventfd_write(sem, 0xffffffffffffffffULL) == -1 && errno == EINVAL);

	/* Level, edge and one-shot over a pipe. */
	int p[2];
	CHECK(pipe(p) == 0);
	int on = 1;
	CHECK(ioctl(p[0], FIONBIO, &on) == 0);
	CHECK(fcntl(p[0], F_GETFL) & O_NONBLOCK);
	ev.events = EPOLLIN;
	ev.data.fd = p[0];
	CHECK(epoll_ctl(ep2, EPOLL_CTL_ADD, p[0], &ev) == 0);
	CHECK(write(p[1], "ab", 2) == 2);
	CHECK(epoll_wait(ep2, out, 4, 0) == 1 && out[0].data.fd == p[0]);
	CHECK(epoll_wait(ep2, out, 4, 0) == 1); /* level: still readable */
	ev.events = EPOLLIN | EPOLLET;
	CHECK(epoll_ctl(ep2, EPOLL_CTL_MOD, p[0], &ev) == 0);
	CHECK(epoll_wait(ep2, out, 4, 0) == 1);
	CHECK(epoll_wait(ep2, out, 4, 0) == 0); /* edge: reported once */
	CHECK(write(p[1], "c", 1) == 1);
	CHECK(epoll_wait(ep2, out, 4, 0) == 1); /* a new write is a new edge */
	ev.events = EPOLLIN | EPOLLONESHOT;
	CHECK(epoll_ctl(ep2, EPOLL_CTL_MOD, p[0], &ev) == 0);
	CHECK(epoll_wait(ep2, out, 4, 0) == 1);
	CHECK(write(p[1], "d", 1) == 1);
	CHECK(epoll_wait(ep2, out, 4, 0) == 0); /* one-shot: disarmed */
	CHECK(epoll_ctl(ep2, EPOLL_CTL_DEL, p[0], 0) == 0);
	CHECK(epoll_wait(ep2, out, 4, 0) == 0);

	/* The timed waits time out, with and without a mask. */
	sigset_t mask;
	sigemptyset(&mask);
	sigaddset(&mask, SIGUSR1);
	CHECK(epoll_pwait(ep2, out, 4, 10, &mask) == 0);
	struct timespec ts = { 0, 10 * 1000 * 1000 };
	CHECK(epoll_pwait2(ep2, out, 4, &ts, 0) == 0);
	CHECK(epoll_pwait2(ep2, out, 4, &ts, &mask) == 0);
	CHECK(epoll_wait(ep2, out, 0, 0) == -1 && errno == EINVAL);

	close(p[0]);
	close(p[1]);
	close(sem);
	close(wake_fd);
	close(ep2);
	close(ep);
	return t_status;
}
