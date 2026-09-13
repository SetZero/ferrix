/*
 * The signals the library reserves, 32 to 34, and signal numbers that do not
 * exist; and the GNU set operations.
 *
 * The first part is adapted from libc-test's
 * src/regression/sigprocmask-internal.c (MIT).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <string.h>
#include "check.h"

int main(void)
{
	sigset_t s;
	int i;

	CHECK(SIGRTMIN == 35);
	CHECK(SIGRTMAX == 64);

	/* libc-test: implementation signals cannot be added or blocked. */
	sigemptyset(&s);
	for (i = 32; i < SIGRTMIN; i++) {
		errno = 0;
		CHECK(sigaddset(&s, i) == -1);
		CHECK(errno == EINVAL);
		CHECK(sigismember(&s, i) == 0);
		errno = 0;
		CHECK(sigdelset(&s, i) == -1);
		CHECK(errno == EINVAL);
	}
	CHECK(sigprocmask(SIG_BLOCK, &s, 0) == 0);
	CHECK(sigprocmask(SIG_BLOCK, 0, &s) == 0);
	for (i = 32; i < SIGRTMIN; i++)
		CHECK(sigismember(&s, i) == 0);

	/* A full set has every signal but the reserved ones. */
	CHECK(sigfillset(&s) == 0);
	for (i = 1; i <= 64; i++)
		CHECK(sigismember(&s, i) == (i < 32 || i > 34));

	/*
	 * A set built by hand reaches the kernel as it is, but the old mask
	 * never shows the reserved signals, as in musl.
	 */
	memset(&s, 0xff, sizeof s);
	CHECK(sigprocmask(SIG_SETMASK, &s, 0) == 0);
	CHECK(sigprocmask(SIG_SETMASK, 0, &s) == 0);
	CHECK(sigismember(&s, 31) == 1);
	CHECK(sigismember(&s, 35) == 1);
	CHECK(sigismember(&s, 64) == 1);
	for (i = 32; i <= 34; i++)
		CHECK(sigismember(&s, i) == 0);
	/* The kernel never blocks SIGKILL or SIGSTOP. */
	CHECK(sigismember(&s, SIGKILL) == 0);
	CHECK(sigismember(&s, SIGSTOP) == 0);
	memset(&s, 0x5a, sizeof s);
	CHECK(pthread_sigmask(SIG_BLOCK, 0, &s) == 0);
	CHECK(sigismember(&s, 33) == 0);
	CHECK(sigismember(&s, SIGUSR1) == 1);
	/* Only the kernel's word of the old mask holds anything. */
	CHECK(s.__bits[1] == 0 && s.__bits[15] == 0);
	sigemptyset(&s);
	CHECK(sigprocmask(SIG_SETMASK, &s, 0) == 0);

	/* sigaction refuses the reserved signals, even to read them. */
	struct sigaction sa;
	sa.sa_handler = SIG_IGN;
	sigemptyset(&sa.sa_mask);
	sa.sa_flags = 0;
	for (i = 32; i <= 34; i++) {
		errno = 0;
		CHECK(sigaction(i, &sa, 0) == -1);
		CHECK(errno == EINVAL);
		errno = 0;
		CHECK(sigaction(i, 0, &sa) == -1);
		CHECK(errno == EINVAL);
	}
	CHECK(signal(33, SIG_IGN) == SIG_ERR);
	CHECK(sigaction(31, &sa, 0) == 0);
	CHECK(sigaction(35, &sa, 0) == 0);
	CHECK(sigaction(64, &sa, 0) == 0);

	/* Signal numbers that do not exist. */
	static const int bad[] = { 0, -1, 65, 1000 };
	for (i = 0; i < (int)(sizeof bad / sizeof bad[0]); i++) {
		errno = 0;
		CHECK(sigaddset(&s, bad[i]) == -1);
		CHECK(errno == EINVAL);
		errno = 0;
		CHECK(sigdelset(&s, bad[i]) == -1);
		CHECK(errno == EINVAL);
		errno = 0;
		CHECK(sigismember(&s, bad[i]) == -1);
		CHECK(errno == EINVAL);
		errno = 0;
		CHECK(sigaction(bad[i], &sa, 0) == -1);
		CHECK(errno == EINVAL);
	}

	/* sigemptyset clears every byte, so equal sets compare equal. */
	sigset_t a, b, c;
	memset(&a, 0x55, sizeof a);
	CHECK(sigemptyset(&a) == 0);
	memset(&b, 0, sizeof b);
	CHECK(memcmp(&a, &b, sizeof a) == 0);

	/* sigisemptyset, sigorset and sigandset. */
	CHECK(sigisemptyset(&a) == 1);
	sigaddset(&a, SIGINT);
	CHECK(sigisemptyset(&a) == 0);
	sigaddset(&b, SIGINT);
	sigaddset(&b, SIGTERM);
	CHECK(sigorset(&c, &a, &b) == 0);
	CHECK(sigismember(&c, SIGINT) == 1);
	CHECK(sigismember(&c, SIGTERM) == 1);
	CHECK(sigismember(&c, SIGHUP) == 0);
	CHECK(sigandset(&c, &a, &b) == 0);
	CHECK(sigismember(&c, SIGINT) == 1);
	CHECK(sigismember(&c, SIGTERM) == 0);

	return t_status;
}
