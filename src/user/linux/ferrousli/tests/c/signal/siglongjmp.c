/*
 * siglongjmp out of signal handlers: with the mask restored and without, out
 * of a real fault on the alternate stack, and with a signal the restored mask
 * lets in.
 */

#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include "check.h"

static sigjmp_buf env;
static volatile int handler_ran, usr2_calls, usr2_before_jump;
static int *volatile bad;

static void jump_out(int sig)
{
	handler_ran = sig;
	siglongjmp(env, sig);
}

static void on_usr2(int sig)
{
	(void)sig;
	usr2_calls++;
}

/* SIGUSR2 is blocked in here; it becomes pending, and the jump lets it in. */
static void jump_with_pending(int sig)
{
	(void)sig;
	raise(SIGUSR2);
	usr2_before_jump = usr2_calls;
	siglongjmp(env, 7);
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
	sigset_t set;
	int r;

	sa.sa_handler = jump_out;
	sigemptyset(&sa.sa_mask);
	sa.sa_flags = 0;
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);

	/* With the mask saved, the signal the handler blocked is unblocked. */
	r = sigsetjmp(env, 1);
	if (r == 0) {
		raise(SIGUSR1);
		CHECK(!"raise returned");
	}
	CHECK(r == SIGUSR1);
	CHECK(handler_ran == SIGUSR1);
	CHECK(blocked(SIGUSR1) == 0);

	/* Without, it stays blocked, as the kernel left it for the handler. */
	handler_ran = 0;
	r = sigsetjmp(env, 0);
	if (r == 0) {
		raise(SIGUSR1);
		CHECK(!"raise returned");
	}
	CHECK(r == SIGUSR1);
	CHECK(handler_ran == SIGUSR1);
	CHECK(blocked(SIGUSR1) == 1);
	sigemptyset(&set);
	CHECK(sigprocmask(SIG_SETMASK, &set, 0) == 0);

	/* Out of a real fault, handled on the alternate stack. */
	static char alt[SIGSTKSZ];
	stack_t ss, now;
	ss.ss_sp = alt;
	ss.ss_size = sizeof alt;
	ss.ss_flags = 0;
	CHECK(sigaltstack(&ss, 0) == 0);
	sa.sa_flags = SA_ONSTACK;
	CHECK(sigaction(SIGSEGV, &sa, 0) == 0);
	for (int round = 0; round < 2; round++) {
		handler_ran = 0;
		r = sigsetjmp(env, 1);
		if (r == 0) {
			*bad = round;
			CHECK(!"the write faulted");
		}
		CHECK(r == SIGSEGV);
		CHECK(handler_ran == SIGSEGV);
		CHECK(blocked(SIGSEGV) == 0);
		/* The alternate stack is no longer in use. */
		CHECK(sigaltstack(0, &now) == 0);
		CHECK(now.ss_flags == 0);
	}

	/*
	 * A signal that became pending inside the handler is delivered when
	 * siglongjmp restores the mask, before the jump lands.
	 */
	sa.sa_handler = on_usr2;
	sa.sa_flags = 0;
	CHECK(sigaction(SIGUSR2, &sa, 0) == 0);
	sa.sa_handler = jump_with_pending;
	sigaddset(&sa.sa_mask, SIGUSR2);
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);
	r = sigsetjmp(env, 1);
	if (r == 0) {
		raise(SIGUSR1);
		CHECK(!"raise returned");
	}
	CHECK(r == 7);
	CHECK(usr2_before_jump == 0);
	CHECK(usr2_calls == 1);
	CHECK(blocked(SIGUSR2) == 0);

	return t_status;
}
