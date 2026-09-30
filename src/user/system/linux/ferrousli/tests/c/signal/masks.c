/*
 * The signal mask: a blocked signal waits in sigpending and is delivered when
 * unblocked; sigprocmask's and pthread_sigmask's errors; sigsuspend; and the
 * System V calls sighold, sigrelse and sigpause.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include "check.h"

static volatile sig_atomic_t delivered;

static void handler(int sig)
{
	(void)sig;
	delivered++;
}

static int blocked(int sig)
{
	sigset_t now;
	sigprocmask(SIG_BLOCK, 0, &now);
	return sigismember(&now, sig);
}

int main(void)
{
	struct sigaction sa;
	sigset_t set, old, pending, empty;

	sigemptyset(&empty);
	sa.sa_handler = handler;
	sigemptyset(&sa.sa_mask);
	sa.sa_flags = 0;
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);
	CHECK(sigaction(SIGUSR2, &sa, 0) == 0);

	/* A blocked signal is pending, not delivered. */
	sigemptyset(&set);
	sigaddset(&set, SIGUSR1);
	CHECK(sigprocmask(SIG_BLOCK, &set, &old) == 0);
	CHECK(sigismember(&old, SIGUSR1) == 0);
	CHECK(raise(SIGUSR1) == 0);
	CHECK(delivered == 0);
	CHECK(sigpending(&pending) == 0);
	CHECK(sigismember(&pending, SIGUSR1) == 1);
	CHECK(sigismember(&pending, SIGUSR2) == 0);

	/* Unblocking it delivers it before sigprocmask returns. */
	CHECK(sigprocmask(SIG_UNBLOCK, &set, 0) == 0);
	CHECK(delivered == 1);
	CHECK(sigpending(&pending) == 0);
	CHECK(sigisemptyset(&pending) == 1);

	/* SIG_SETMASK replaces the mask, and a null set only reads it. */
	sigaddset(&set, SIGUSR2);
	CHECK(sigprocmask(SIG_SETMASK, &set, 0) == 0);
	CHECK(sigprocmask(SIG_BLOCK, 0, &old) == 0);
	CHECK(sigismember(&old, SIGUSR1) == 1);
	CHECK(sigismember(&old, SIGUSR2) == 1);
	CHECK(sigprocmask(12345, 0, &old) == 0);
	CHECK(sigprocmask(SIG_SETMASK, &empty, &old) == 0);
	CHECK(sigismember(&old, SIGUSR2) == 1);
	CHECK(blocked(SIGUSR2) == 0);

	/* A bad how fails with EINVAL and changes nothing. */
	errno = 0;
	CHECK(sigprocmask(3, &set, 0) == -1);
	CHECK(errno == EINVAL);
	errno = 0;
	CHECK(sigprocmask(-1, &set, 0) == -1);
	CHECK(errno == EINVAL);
	CHECK(blocked(SIGUSR1) == 0);

	/* pthread_sigmask returns the error and leaves errno alone. */
	errno = 0;
	CHECK(pthread_sigmask(99, &set, 0) == EINVAL);
	CHECK(errno == 0);
	CHECK(pthread_sigmask(SIG_BLOCK, &set, 0) == 0);
	CHECK(pthread_sigmask(SIG_UNBLOCK, &set, &old) == 0);
	CHECK(sigismember(&old, SIGUSR2) == 1);
	CHECK(blocked(SIGUSR2) == 0);

	/* Bad pointers fail with EFAULT rather than crashing. */
	errno = 0;
	CHECK(sigprocmask(SIG_BLOCK, (sigset_t *)8, 0) == -1);
	CHECK(errno == EFAULT);
	errno = 0;
	CHECK(sigprocmask(SIG_BLOCK, 0, (sigset_t *)8) == -1);
	CHECK(errno == EFAULT);
	CHECK(pthread_sigmask(SIG_BLOCK, (sigset_t *)8, 0) == EFAULT);
	errno = 0;
	CHECK(sigpending((sigset_t *)8) == -1);
	CHECK(errno == EFAULT);
	errno = 0;
	CHECK(sigsuspend((sigset_t *)8) == -1);
	CHECK(errno == EFAULT);

	/* sigsuspend waits with a mask that lets the pending signal in. */
	delivered = 0;
	sigemptyset(&set);
	sigaddset(&set, SIGUSR1);
	CHECK(sigprocmask(SIG_SETMASK, &set, 0) == 0);
	CHECK(raise(SIGUSR1) == 0);
	CHECK(delivered == 0);
	errno = 0;
	CHECK(sigsuspend(&empty) == -1);
	CHECK(errno == EINTR);
	CHECK(delivered == 1);
	CHECK(blocked(SIGUSR1) == 1);
	CHECK(sigprocmask(SIG_SETMASK, &empty, 0) == 0);

	/* sighold and sigrelse. */
	CHECK(sighold(SIGUSR2) == 0);
	CHECK(blocked(SIGUSR2) == 1);
	CHECK(raise(SIGUSR2) == 0);
	CHECK(delivered == 1);
	CHECK(sigrelse(SIGUSR2) == 0);
	CHECK(delivered == 2);
	CHECK(blocked(SIGUSR2) == 0);
	errno = 0;
	CHECK(sighold(33) == -1);
	CHECK(errno == EINVAL);
	errno = 0;
	CHECK(sigrelse(0) == -1);
	CHECK(errno == EINVAL);

	/* sigpause unblocks the signal while it waits. */
	CHECK(sighold(SIGUSR2) == 0);
	CHECK(raise(SIGUSR2) == 0);
	errno = 0;
	CHECK(sigpause(SIGUSR2) == -1);
	CHECK(errno == EINTR);
	CHECK(delivered == 3);
	CHECK(blocked(SIGUSR2) == 1);

	return t_status;
}
