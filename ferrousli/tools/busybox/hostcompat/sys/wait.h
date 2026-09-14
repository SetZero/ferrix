/* For docproc and mconf, which the busybox build compiles but never runs. */
#ifndef HOSTCOMPAT_SYS_WAIT_H
#define HOSTCOMPAT_SYS_WAIT_H
#include <errno.h>
#include <sys/types.h>

#define WIFEXITED(s) (((s) & 0x7f) == 0)
#define WEXITSTATUS(s) (((s) >> 8) & 0xff)
#define WIFSIGNALED(s) (0)
#define WTERMSIG(s) (0)

static inline int fork(void)
{
	errno = ENOSYS;
	return -1;
}

static inline int waitpid(int pid, int *status, int options)
{
	(void)pid; (void)status; (void)options;
	errno = ENOSYS;
	return -1;
}
#endif
