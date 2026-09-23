/*
 * Randomness and identity: getrandom, getentropy, uname, gethostname, isatty,
 * and the user and group ids.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <string.h>
#include <sys/random.h>
#include <sys/utsname.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	unsigned char a[32] = { 0 }, b[32] = { 0 }, big[257];
	struct utsname u;
	char host[256];
	gid_t groups[256];
	int p[2], count;
	size_t n;

	/* getrandom. */
	CHECK(getrandom(a, sizeof a, 0) == sizeof a);
	CHECK(getrandom(b, sizeof b, GRND_NONBLOCK) == sizeof b);
	CHECK(memcmp(a, b, sizeof a) != 0);
	CHECK(getrandom(a, 0, 0) == 0);
	errno = 0;
	CHECK(getrandom(a, sizeof a, 0x100) == -1 && errno == EINVAL);

	/* getentropy, up to 256 bytes and no more. */
	memset(big, 0, sizeof big);
	CHECK(getentropy(big, 256) == 0);
	CHECK(memcmp(big, big + 128, 128) != 0);
	CHECK(getentropy(big, 0) == 0);
	errno = 0;
	CHECK(getentropy(big, 257) == -1 && errno == EIO);

	/* uname and gethostname. */
	CHECK(uname(&u) == 0);
	/* Linux on the host; Ferrix reports its own name. */
	CHECK(strcmp(u.sysname, "Linux") == 0 || strcmp(u.sysname, "Ferrix") == 0);
#if defined(__x86_64__)
	CHECK(strcmp(u.machine, "x86_64") == 0);
#elif defined(__aarch64__)
	CHECK(strcmp(u.machine, "aarch64") == 0);
#elif defined(__arm__)
	CHECK(strncmp(u.machine, "armv7", 5) == 0);
#endif
	CHECK(u.release[0] != 0);
	CHECK(gethostname(host, sizeof host) == 0 && strcmp(host, u.nodename) == 0);
	n = strlen(u.nodename);
	if (n > 0) {
		memset(host, 'x', sizeof host);
		errno = 0;
		CHECK(gethostname(host, n) == -1 && errno == ENAMETOOLONG && host[n - 1] == 0);
		CHECK(gethostname(host, n + 1) == 0 && strcmp(host, u.nodename) == 0);
	}

	/* A pipe is not a terminal. */
	CHECK(pipe(p) == 0);
	errno = 0;
	CHECK(isatty(p[0]) == 0 && errno == ENOTTY);
	errno = 0;
	CHECK(isatty(p[1]) == 0 && errno == ENOTTY);

	/* Process ids. */
	CHECK(getpid() > 0 && getppid() > 0 && getpid() != getppid());

	/* The ids of a program that is not set-id, and setting them unchanged. */
	CHECK(getuid() == geteuid() && getgid() == getegid());
	CHECK(setuid(getuid()) == 0 && setgid(getgid()) == 0);
	CHECK(seteuid(geteuid()) == 0 && setegid(getegid()) == 0);
	CHECK(setreuid(-1, -1) == 0 && setregid(-1, -1) == 0);
	CHECK(getuid() == geteuid() && getgid() == getegid());
	if (getuid() != 0) {
		errno = 0;
		CHECK(setuid(0) == -1 && errno == EPERM);
		errno = 0;
		CHECK(seteuid(0) == -1 && errno == EPERM);
	}

	/* Supplementary groups: counted, read, and too many for the buffer. */
	count = getgroups(0, 0);
	CHECK(count >= 0);
	if (count > 0 && count <= 256) {
		CHECK(getgroups(count, groups) == count);
		if (count > 1) {
			errno = 0;
			CHECK(getgroups(count - 1, groups) == -1 && errno == EINVAL);
		}
		/* Setting them takes privilege; as root they are set unchanged. */
		errno = 0;
		CHECK(setgroups(count, groups) == 0 || errno == EPERM);
	}

	return t_status;
}
