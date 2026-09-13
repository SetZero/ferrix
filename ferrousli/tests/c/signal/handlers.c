/*
 * Handlers run for raise and kill, SA_SIGINFO reports the sender, sigaction
 * reports the action it replaces, and signal, bsd_signal, siginterrupt,
 * sigignore and sigset change dispositions as musl's do.
 *
 * The end is adapted from libc-test's src/regression/sigreturn.c (MIT).
 *
 * There is no getpid yet, so the process id comes from the siginfo of a
 * signal raise sent.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include "check.h"

static volatile sig_atomic_t usr1_calls, usr2_calls;
static volatile int info_signo, info_code, info_pid;

static void on_usr1(int sig)
{
	if (sig == SIGUSR1)
		usr1_calls++;
}

static void on_usr2(int sig)
{
	if (sig == SIGUSR2)
		usr2_calls++;
}

static void with_info(int sig, siginfo_t *si, void *context)
{
	info_signo = si->si_signo;
	info_code = si->si_code;
	info_pid = si->si_pid;
	(void)sig;
	(void)context;
}

static volatile sig_atomic_t sigreturn_x;

static void sigreturn_handler(int s)
{
	(void)s;
	sigreturn_x = 1;
}

int main(void)
{
	struct sigaction sa, old;

	/* The handler has run by the time raise returns. */
	sigemptyset(&sa.sa_mask);
	sa.sa_flags = 0;
	sa.sa_handler = on_usr1;
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);
	CHECK(raise(SIGUSR1) == 0);
	CHECK(usr1_calls == 1);

	/* SA_SIGINFO, and the previous action coming back. */
	sa.sa_sigaction = with_info;
	sa.sa_flags = SA_SIGINFO;
	sigaddset(&sa.sa_mask, SIGUSR2);
	CHECK(sigaction(SIGUSR1, &sa, &old) == 0);
	CHECK(old.sa_handler == on_usr1);
	CHECK((old.sa_flags & SA_SIGINFO) == 0);
	CHECK(sigismember(&old.sa_mask, SIGUSR2) == 0);

	CHECK(raise(SIGUSR1) == 0);
	CHECK(info_signo == SIGUSR1);
	CHECK(info_code == SI_TKILL);
	CHECK(info_pid > 0);
	pid_t pid = info_pid;

	CHECK(sigaction(SIGUSR1, 0, &old) == 0);
	CHECK(old.sa_sigaction == with_info);
	CHECK((old.sa_flags & SA_SIGINFO) != 0);
	CHECK(sigismember(&old.sa_mask, SIGUSR2) == 1);
	CHECK(sigismember(&old.sa_mask, SIGINT) == 0);
	CHECK(usr1_calls == 1);

	/* kill to this process runs the handler before it returns too. */
	info_signo = info_code = info_pid = 0;
	CHECK(kill(pid, SIGUSR1) == 0);
	CHECK(info_signo == SIGUSR1);
	CHECK(info_code == SI_USER);
	CHECK(info_pid == pid);

	/* Signal 0 checks only that the process exists. */
	info_signo = 0;
	CHECK(kill(pid, 0) == 0);
	CHECK(raise(0) == 0);
	CHECK(info_signo == 0);

	errno = 0;
	CHECK(kill(pid, 65) == -1);
	CHECK(errno == EINVAL);
	errno = 0;
	CHECK(raise(-1) == -1);
	CHECK(errno == EINVAL);
	errno = 0;
	CHECK(killpg(-1, SIGUSR1) == -1);
	CHECK(errno == EINVAL);

	/* signal: the handler stays installed, and calls restart. */
	CHECK(signal(SIGUSR2, on_usr2) == SIG_DFL);
	CHECK(raise(SIGUSR2) == 0);
	CHECK(raise(SIGUSR2) == 0);
	CHECK(usr2_calls == 2);
	CHECK(sigaction(SIGUSR2, 0, &old) == 0);
	CHECK(old.sa_handler == on_usr2);
	CHECK((old.sa_flags & SA_RESTART) != 0);
	CHECK((old.sa_flags & (SA_RESETHAND | SA_NODEFER | SA_SIGINFO)) == 0);

	CHECK(bsd_signal(SIGUSR2, SIG_IGN) == on_usr2);
	CHECK(raise(SIGUSR2) == 0);
	CHECK(usr2_calls == 2);
	CHECK(signal(SIGUSR2, on_usr2) == SIG_IGN);

	errno = 0;
	CHECK(signal(SIGKILL, on_usr1) == SIG_ERR);
	CHECK(errno == EINVAL);
	errno = 0;
	CHECK(signal(0, on_usr1) == SIG_ERR);
	CHECK(errno == EINVAL);
	errno = 0;
	CHECK(signal(NSIG, on_usr1) == SIG_ERR);
	CHECK(errno == EINVAL);
	errno = 0;
	CHECK(sigaction(SIGSTOP, &sa, 0) == -1);
	CHECK(errno == EINVAL);
	/* Reading SIGKILL's action is allowed. */
	CHECK(sigaction(SIGKILL, 0, &old) == 0);
	CHECK(old.sa_handler == SIG_DFL);

	/* siginterrupt turns SA_RESTART off and on, and keeps the handler. */
	CHECK(siginterrupt(SIGUSR2, 1) == 0);
	CHECK(sigaction(SIGUSR2, 0, &old) == 0);
	CHECK((old.sa_flags & SA_RESTART) == 0);
	CHECK(old.sa_handler == on_usr2);
	CHECK(siginterrupt(SIGUSR2, 0) == 0);
	CHECK(sigaction(SIGUSR2, 0, &old) == 0);
	CHECK((old.sa_flags & SA_RESTART) != 0);
	CHECK(old.sa_handler == on_usr2);
	errno = 0;
	CHECK(siginterrupt(0, 1) == -1);
	CHECK(errno == EINVAL);

	/* sigignore. */
	CHECK(sigignore(SIGUSR2) == 0);
	CHECK(sigaction(SIGUSR2, 0, &old) == 0);
	CHECK(old.sa_handler == SIG_IGN);
	errno = 0;
	CHECK(sigignore(SIGKILL) == -1);
	CHECK(errno == EINVAL);

	/* sigset installs and unblocks, or with SIG_HOLD blocks. */
	sigset_t mask;
	CHECK(sigset(SIGUSR2, on_usr2) == SIG_IGN);
	CHECK(sigset(SIGUSR2, SIG_HOLD) == on_usr2);
	CHECK(sigprocmask(SIG_BLOCK, 0, &mask) == 0);
	CHECK(sigismember(&mask, SIGUSR2) == 1);
	CHECK(sigset(SIGUSR2, SIG_HOLD) == SIG_HOLD);
	CHECK(raise(SIGUSR2) == 0);
	CHECK(usr2_calls == 2);
	/* Unblocking it delivers the signal that was held. */
	CHECK(sigset(SIGUSR2, on_usr2) == SIG_HOLD);
	CHECK(usr2_calls == 3);
	CHECK(sigprocmask(SIG_BLOCK, 0, &mask) == 0);
	CHECK(sigismember(&mask, SIGUSR2) == 0);
	CHECK(sigset(SIGUSR2, SIG_DFL) == on_usr2);
	errno = 0;
	CHECK(sigset(33, on_usr2) == SIG_ERR);
	CHECK(errno == EINVAL);

	/* libc-test's sigreturn regression: returning from a handler. */
	signal(SIGINT, sigreturn_handler);
	CHECK(raise(SIGINT) == 0);
	CHECK(sigreturn_x == 1);

	return t_status;
}
