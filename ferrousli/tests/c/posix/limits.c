/*
 * Limits: sysconf, pathconf, resource limits and usage, and scheduling priority.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <sys/resource.h>
#include <unistd.h>

#include "check.h"

/* What sysconf should report for a resource limit. */
static long limit_value(int resource)
{
	struct rlimit lim;
	if (getrlimit(resource, &lim) != 0 || lim.rlim_cur == RLIM_INFINITY)
		return -1;
	return lim.rlim_cur > LONG_MAX ? LONG_MAX : (long)lim.rlim_cur;
}

int main(void)
{
	struct rlimit old, now;
	struct rusage ru;
	int prio;

	/* sysconf. */
	CHECK(sysconf(_SC_PAGESIZE) == getpagesize());
	CHECK(sysconf(_SC_PAGE_SIZE) == 4096);
	CHECK(sysconf(_SC_CLK_TCK) == 100);
	CHECK(sysconf(_SC_NPROCESSORS_CONF) >= 1 && sysconf(_SC_NPROCESSORS_ONLN) >= 1);
	CHECK(sysconf(_SC_ARG_MAX) >= _POSIX_ARG_MAX);
	CHECK(sysconf(_SC_HOST_NAME_MAX) == HOST_NAME_MAX);
	CHECK(sysconf(_SC_LOGIN_NAME_MAX) == LOGIN_NAME_MAX);
	CHECK(sysconf(_SC_PHYS_PAGES) > 0);
	CHECK(sysconf(_SC_OPEN_MAX) == limit_value(RLIMIT_NOFILE));
	CHECK(sysconf(_SC_CHILD_MAX) == limit_value(RLIMIT_NPROC));
	errno = 0;
	CHECK(sysconf(-1) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(sysconf(100000) == -1 && errno == EINVAL);

	/* pathconf and fpathconf: musl's table, whatever the file. */
	CHECK(pathconf("/", _PC_NAME_MAX) == NAME_MAX);
	CHECK(fpathconf(0, _PC_PATH_MAX) == PATH_MAX);
	CHECK(pathconf("/", _PC_PIPE_BUF) == PIPE_BUF);
	errno = 0;
	CHECK(fpathconf(0, _PC_SYMLINK_MAX) == -1 && errno == 0);
	CHECK(fpathconf(0, -1) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(pathconf("/", _PC_2_SYMLINKS + 1) == -1 && errno == EINVAL);

	/* A round trip on RLIMIT_NOFILE's soft limit. */
	CHECK(getrlimit(RLIMIT_NOFILE, &old) == 0 && old.rlim_cur <= old.rlim_max);
	if (old.rlim_max >= 64) {
		now = old;
		now.rlim_cur = 64;
		CHECK(setrlimit(RLIMIT_NOFILE, &now) == 0);
		CHECK(getrlimit(RLIMIT_NOFILE, &now) == 0);
		CHECK(now.rlim_cur == 64 && now.rlim_max == old.rlim_max);
		CHECK(sysconf(_SC_OPEN_MAX) == 64);
		/* The lowered limit applies. */
		errno = 0;
		CHECK(dup2(0, 64) == -1 && errno == EBADF);
		CHECK(dup2(0, 63) == 63 && close(63) == 0);
		CHECK(setrlimit(RLIMIT_NOFILE, &old) == 0);
		CHECK(getrlimit(RLIMIT_NOFILE, &now) == 0);
		CHECK(now.rlim_cur == old.rlim_cur && now.rlim_max == old.rlim_max);
	}
	/* The soft limit cannot pass the hard one. */
	if (old.rlim_max != RLIM_INFINITY) {
		now.rlim_cur = old.rlim_max + 1;
		now.rlim_max = old.rlim_max;
		errno = 0;
		CHECK(setrlimit(RLIMIT_NOFILE, &now) == -1 && errno == EINVAL);
	}
	errno = 0;
	CHECK(getrlimit(RLIMIT_NLIMITS + 100, &now) == -1 && errno == EINVAL);
	CHECK(prlimit(0, RLIMIT_NOFILE, 0, &now) == 0 && now.rlim_cur == old.rlim_cur);
	CHECK(prlimit(getpid(), RLIMIT_NOFILE, &old, &now) == 0 && now.rlim_cur == old.rlim_cur);

	/* Usage. */
	CHECK(getrusage(RUSAGE_SELF, &ru) == 0 && ru.ru_maxrss > 0);
	CHECK(ru.ru_utime.tv_usec >= 0 && ru.ru_utime.tv_usec < 1000000);
	CHECK(getrusage(RUSAGE_CHILDREN, &ru) == 0);
	errno = 0;
	CHECK(getrusage(12345, &ru) == -1 && errno == EINVAL);

	/* Priority: lowering one's own is always allowed. */
	errno = 0;
	prio = getpriority(PRIO_PROCESS, 0);
	CHECK(errno == 0 && prio >= -20 && prio <= 19);
	if (prio < 19) {
		CHECK(setpriority(PRIO_PROCESS, 0, prio + 1) == 0);
		errno = 0;
		CHECK(getpriority(PRIO_PROCESS, 0) == prio + 1 && errno == 0);
	}
	errno = 0;
	CHECK(getpriority(12345, 0) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(setpriority(PRIO_PROCESS, 0x7fffffff, 0) == -1 && errno == ESRCH);

	return t_status;
}
