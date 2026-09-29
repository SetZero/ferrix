/*
 * Waiting for signals: sigtimedwait timing out and returning a pending signal,
 * sigwaitinfo, sigwait, and sigqueue carrying a value to a waiter and to a
 * handler.
 *
 * There is no getpid yet, so the process id comes from the siginfo of a
 * signal raise sent.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include "check.h"

static volatile int handler_code;
static void *volatile handler_ptr;

static void on_queue(int sig, siginfo_t *si, void *context)
{
	(void)sig;
	(void)context;
	handler_code = si->si_code;
	handler_ptr = si->si_value.sival_ptr;
}

int main(void)
{
	sigset_t set, pending;
	siginfo_t si;
	struct timespec ts;

	sigemptyset(&set);
	sigaddset(&set, SIGUSR1);
	sigaddset(&set, SIGUSR2);
	CHECK(sigprocmask(SIG_BLOCK, &set, 0) == 0);

	/* Nothing pending: the wait times out with EAGAIN. */
	ts.tv_sec = 0;
	ts.tv_nsec = 20 * 1000 * 1000;
	errno = 0;
	CHECK(sigtimedwait(&set, &si, &ts) == -1);
	CHECK(errno == EAGAIN);
	ts.tv_nsec = 0;
	errno = 0;
	CHECK(sigtimedwait(&set, &si, &ts) == -1);
	CHECK(errno == EAGAIN);

	/* A timeout the kernel cannot read is EINVAL. */
	ts.tv_nsec = 1000 * 1000 * 1000;
	errno = 0;
	CHECK(sigtimedwait(&set, &si, &ts) == -1);
	CHECK(errno == EINVAL);

	/* A pending signal returns at once, and is no longer pending. */
	CHECK(raise(SIGUSR2) == 0);
	ts.tv_sec = 5;
	ts.tv_nsec = 0;
	CHECK(sigtimedwait(&set, &si, &ts) == SIGUSR2);
	CHECK(si.si_signo == SIGUSR2);
	CHECK(si.si_code == SI_TKILL);
	CHECK(si.si_pid > 0);
	pid_t pid = si.si_pid;
	CHECK(sigpending(&pending) == 0);
	CHECK(sigisemptyset(&pending) == 1);

	/* Without a siginfo. */
	CHECK(raise(SIGUSR1) == 0);
	CHECK(sigtimedwait(&set, 0, &ts) == SIGUSR1);

	/* sigwaitinfo and sigwait. */
	CHECK(raise(SIGUSR1) == 0);
	CHECK(sigwaitinfo(&set, &si) == SIGUSR1);
	CHECK(si.si_signo == SIGUSR1);
	int sig = 0;
	CHECK(raise(SIGUSR2) == 0);
	CHECK(sigwait(&set, &sig) == 0);
	CHECK(sig == SIGUSR2);

	/* sigwait returns an error number, as POSIX says. */
	errno = 0;
	CHECK(sigwait((sigset_t *)8, &sig) == EFAULT);
	CHECK(errno == 0);

	/* sigqueue's value reaches a waiter, with SI_QUEUE and the sender. */
	union sigval value;
	value.sival_int = 4242;
	CHECK(sigqueue(pid, SIGUSR1, value) == 0);
	CHECK(sigwaitinfo(&set, &si) == SIGUSR1);
	CHECK(si.si_code == SI_QUEUE);
	CHECK(si.si_value.sival_int == 4242);
	CHECK(si.si_pid == pid);

	/* A real-time signal queues each value, in order. */
	int rt = SIGRTMIN + 1;
	sigset_t rtset;
	sigemptyset(&rtset);
	CHECK(sigaddset(&rtset, rt) == 0);
	CHECK(sigprocmask(SIG_BLOCK, &rtset, 0) == 0);
	value.sival_int = 1;
	CHECK(sigqueue(pid, rt, value) == 0);
	value.sival_int = 2;
	CHECK(sigqueue(pid, rt, value) == 0);
	CHECK(sigwaitinfo(&rtset, &si) == rt);
	CHECK(si.si_value.sival_int == 1);
	CHECK(sigwaitinfo(&rtset, &si) == rt);
	CHECK(si.si_value.sival_int == 2);

	/* And a pointer reaches a handler. */
	static int marker;
	struct sigaction sa;
	sa.sa_sigaction = on_queue;
	sa.sa_flags = SA_SIGINFO;
	sigemptyset(&sa.sa_mask);
	CHECK(sigaction(SIGUSR2, &sa, 0) == 0);
	value.sival_ptr = &marker;
	CHECK(sigqueue(pid, SIGUSR2, value) == 0);
	CHECK(handler_ptr == 0);
	CHECK(sigprocmask(SIG_UNBLOCK, &set, 0) == 0);
	CHECK(handler_ptr == &marker);
	CHECK(handler_code == SI_QUEUE);

	errno = 0;
	CHECK(sigqueue(pid, 65, value) == -1);
	CHECK(errno == EINVAL);

	return t_status;
}
