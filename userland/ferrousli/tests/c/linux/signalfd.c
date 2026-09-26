/*
 * signalfd as glib's and Chrome's event loops use it: blocked signals read
 * through a descriptor in an epoll set. A signal of the mask makes it
 * readable and reads back as one signalfd_siginfo naming the signal, SI_USER
 * and the sender, and is then gone; a signal outside the mask stays pending
 * until the mask is changed to hold it; two pending come back in one read,
 * lowest first; a blocked read is woken by a signal a child sends; and the
 * errors.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

_Static_assert(sizeof(struct signalfd_siginfo) == 128, "signalfd_siginfo is 128 bytes");

static void nap(long nanos)
{
	struct timespec t = { nanos / 1000000000, nanos % 1000000000 };
	while (nanosleep(&t, &t) == -1 && errno == EINTR)
		;
}

static int is_pending(int sig)
{
	sigset_t pending;
	sigemptyset(&pending);
	return sigpending(&pending) == 0 && sigismember(&pending, sig) == 1;
}

int main(void)
{
	struct signalfd_siginfo info[3];
	struct epoll_event event = { .events = EPOLLIN }, ready;
	sigset_t blocked, mask;
	char small[4];
	pid_t child;
	int fd, waiting, epoll, status;

	sigemptyset(&blocked);
	sigaddset(&blocked, SIGUSR1);
	sigaddset(&blocked, SIGUSR2);
	CHECK(sigprocmask(SIG_BLOCK, &blocked, 0) == 0);

	sigemptyset(&mask);
	sigaddset(&mask, SIGUSR1);
	errno = 0;
	CHECK(signalfd(-1, &mask, 0x1234) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(signalfd(12345, &mask, 0) == -1 && errno == EBADF);

	fd = signalfd(-1, &mask, SFD_NONBLOCK | SFD_CLOEXEC);
	CHECK(fd >= 0);
	CHECK(fcntl(fd, F_GETFD) == FD_CLOEXEC);
	CHECK(fcntl(fd, F_GETFL) & O_NONBLOCK);
	errno = 0;
	CHECK(read(fd, info, sizeof info) == -1 && errno == EAGAIN);
	errno = 0;
	CHECK(read(fd, small, sizeof small) == -1 && errno == EINVAL);

	/* One signal of the mask, seen by epoll_wait, read, and gone. */
	epoll = epoll_create1(EPOLL_CLOEXEC);
	CHECK(epoll >= 0 && epoll_ctl(epoll, EPOLL_CTL_ADD, fd, &event) == 0);
	CHECK(epoll_wait(epoll, &ready, 1, 0) == 0);
	CHECK(kill(getpid(), SIGUSR1) == 0);
	CHECK(epoll_wait(epoll, &ready, 1, 5000) == 1 && ready.events & EPOLLIN);
	CHECK(read(fd, info, sizeof info) == sizeof info[0]);
	CHECK(info[0].ssi_signo == SIGUSR1 && info[0].ssi_code == SI_USER);
	CHECK(info[0].ssi_pid == (uint32_t)getpid());
	CHECK(!is_pending(SIGUSR1));
	errno = 0;
	CHECK(read(fd, info, sizeof info) == -1 && errno == EAGAIN);

	/* One outside the mask stays pending, until the mask holds it. */
	CHECK(kill(getpid(), SIGUSR2) == 0);
	errno = 0;
	CHECK(read(fd, info, sizeof info) == -1 && errno == EAGAIN);
	CHECK(is_pending(SIGUSR2));
	sigaddset(&mask, SIGUSR2);
	CHECK(signalfd(fd, &mask, 0) == fd);
	CHECK(read(fd, info, sizeof info) == sizeof info[0] && info[0].ssi_signo == SIGUSR2);

	/* Two pending come back in one read, lowest first. */
	CHECK(kill(getpid(), SIGUSR2) == 0);
	CHECK(kill(getpid(), SIGUSR1) == 0);
	CHECK(read(fd, info, sizeof info) == 2 * sizeof info[0]);
	CHECK(info[0].ssi_signo == SIGUSR1 && info[1].ssi_signo == SIGUSR2);

	/* A blocked read, woken by a signal a child sends. */
	waiting = signalfd(-1, &mask, SFD_CLOEXEC);
	CHECK(waiting >= 0);
	child = fork();
	if (child == 0) {
		nap(50 * 1000 * 1000);
		kill(getppid(), SIGUSR2);
		_exit(0);
	}
	CHECK(child > 0);
	CHECK(read(waiting, info, sizeof info) == sizeof info[0]);
	CHECK(info[0].ssi_signo == SIGUSR2 && info[0].ssi_pid == (uint32_t)child);
	CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status));

	/* A descriptor that is not a signalfd cannot be given a mask. */
	errno = 0;
	CHECK(signalfd(epoll, &mask, 0) == -1 && errno == EINVAL);

	close(waiting);
	close(epoll);
	close(fd);
	return t_status;
}
