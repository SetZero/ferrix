/*
 * sigaction's flags: a signal is blocked inside its own handler unless
 * SA_NODEFER, sa_mask is blocked too, SA_RESETHAND puts the default back, and
 * every flag reads back as it was installed.
 */

#define _GNU_SOURCE
#include <signal.h>
#include "check.h"

static volatile int calls, self_blocked, usr2_blocked;

static void handler(int sig)
{
	sigset_t now;
	calls++;
	sigprocmask(SIG_BLOCK, 0, &now);
	self_blocked = sigismember(&now, sig);
	usr2_blocked = sigismember(&now, SIGUSR2);
}

static int blocked(int sig)
{
	sigset_t now;
	sigprocmask(SIG_BLOCK, 0, &now);
	return sigismember(&now, sig);
}

int main(void)
{
	struct sigaction sa, old;

	/* By default the signal is blocked while its handler runs, with sa_mask. */
	sigemptyset(&sa.sa_mask);
	sigaddset(&sa.sa_mask, SIGUSR2);
	sa.sa_flags = 0;
	sa.sa_handler = handler;
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);
	CHECK(raise(SIGUSR1) == 0);
	CHECK(calls == 1);
	CHECK(self_blocked == 1);
	CHECK(usr2_blocked == 1);
	/* The mask is back once it returns. */
	CHECK(blocked(SIGUSR1) == 0);
	CHECK(blocked(SIGUSR2) == 0);

	/* SA_NODEFER leaves it unblocked. */
	sigemptyset(&sa.sa_mask);
	sa.sa_flags = SA_NODEFER;
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);
	CHECK(raise(SIGUSR1) == 0);
	CHECK(calls == 2);
	CHECK(self_blocked == 0);
	CHECK(usr2_blocked == 0);

	/* SA_RESETHAND: the handler runs once, then the default is back. */
	sa.sa_flags = SA_RESETHAND;
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);
	CHECK(raise(SIGUSR1) == 0);
	CHECK(calls == 3);
	CHECK(sigaction(SIGUSR1, 0, &old) == 0);
	CHECK(old.sa_handler == SIG_DFL);

	/* SIGURG's default is to ignore, so it can be raised a second time. */
	CHECK(sigaction(SIGURG, &sa, 0) == 0);
	CHECK(raise(SIGURG) == 0);
	CHECK(raise(SIGURG) == 0);
	CHECK(calls == 4);

	/* Flags read back as installed, including SA_RESETHAND, bit 31. */
	int flags = SA_RESETHAND | SA_NODEFER | SA_RESTART | SA_ONSTACK | SA_SIGINFO;
	sa.sa_flags = flags;
	CHECK(sigaction(SIGUSR2, &sa, 0) == 0);
	CHECK(sigaction(SIGUSR2, 0, &old) == 0);
	CHECK((old.sa_flags & flags) == flags);
	CHECK((old.sa_flags & SA_NOCLDSTOP) == 0);

	return t_status;
}
