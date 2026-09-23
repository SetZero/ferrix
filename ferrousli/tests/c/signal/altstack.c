/*
 * sigaltstack: a handler with SA_ONSTACK runs on the alternate stack and sees
 * SS_ONSTACK; one without it does not; and the size and flag checks.
 *
 * Adapted from libc-test's src/regression/sigaltstack.c (MIT).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include "check.h"

static char stack[SIGSTKSZ];
static volatile int ran, on_alt, flags_inside, change_errno;

static void handler(int sig)
{
	uintptr_t i;
	stack_t ss;

	(void)sig;
	i = (uintptr_t)&i;
	on_alt = i >= (uintptr_t)stack && i < (uintptr_t)stack + sizeof stack;
	flags_inside = sigaltstack(0, &ss) == 0 ? ss.ss_flags : -1;

	/* The stack in use cannot be changed. */
	ss.ss_sp = stack;
	ss.ss_size = sizeof stack;
	ss.ss_flags = 0;
	errno = 0;
	change_errno = sigaltstack(&ss, 0) == -1 ? errno : 0;
	ran = 1;
}

int main(void)
{
	stack_t ss, old;
	struct sigaction sa;

	CHECK(sigaltstack(0, &old) == 0);
	CHECK(old.ss_flags == SS_DISABLE);

	ss.ss_sp = stack;
	ss.ss_size = sizeof stack;
	ss.ss_flags = 0;
	sa.sa_handler = handler;
	sa.sa_flags = SA_ONSTACK;

	CHECK(sigaltstack(&ss, 0) == 0);
	CHECK(sigfillset(&sa.sa_mask) == 0);
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);
	CHECK(raise(SIGUSR1) == 0);
	CHECK(ran == 1);
	CHECK(on_alt == 1);
	CHECK(flags_inside == SS_ONSTACK);
	CHECK(change_errno == EPERM);

	/* Off the handler, the stack is set up but not in use. */
	CHECK(sigaltstack(0, &old) == 0);
	CHECK(old.ss_sp == stack);
	CHECK(old.ss_size == sizeof stack);
	CHECK(old.ss_flags == 0);

	/* Without SA_ONSTACK the handler runs on the ordinary stack. */
	sa.sa_flags = 0;
	CHECK(sigaction(SIGUSR1, &sa, 0) == 0);
	ran = 0;
	CHECK(raise(SIGUSR1) == 0);
	CHECK(ran == 1);
	CHECK(on_alt == 0);
	CHECK(flags_inside == 0);

#ifndef FERROUSLI_TEST_EMULATED
	/* qemu-user checks against its own, smaller, minimum. */
	errno = 0;
	ss.ss_size = MINSIGSTKSZ - 1;
	CHECK(sigaltstack(&ss, 0) == -1);
	CHECK(errno == ENOMEM);
#endif
	errno = 0;
	ss.ss_flags = -1;
	ss.ss_size = MINSIGSTKSZ;
	CHECK(sigaltstack(&ss, 0) == -1);
	CHECK(errno == EINVAL);
	errno = 0;
	ss.ss_flags = SS_ONSTACK;
	CHECK(sigaltstack(&ss, 0) == -1);
	CHECK(errno == EINVAL);
	errno = 0;
	CHECK(sigaltstack(0, 0) == 0);
	/*
	 * A bad old pointer is the kernel's to refuse. A bad new one faults, in
	 * musl too, since its flags are checked before the call.
	 */
	errno = 0;
	CHECK(sigaltstack(0, (stack_t *)8) == -1);
	CHECK(errno == EFAULT);

	/* Disabling needs no size. */
	ss.ss_flags = SS_DISABLE;
	ss.ss_size = 0;
	CHECK(sigaltstack(&ss, &old) == 0);
	CHECK(old.ss_sp == stack);
	CHECK(sigaltstack(0, &old) == 0);
	CHECK(old.ss_flags == SS_DISABLE);

	return t_status;
}
