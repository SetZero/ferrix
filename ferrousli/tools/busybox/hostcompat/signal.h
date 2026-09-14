/* mingw's signal.h plus the POSIX pieces mconf names; it is linked, never run. */
#ifndef HOSTCOMPAT_SIGNAL_H
#define HOSTCOMPAT_SIGNAL_H
#include_next <signal.h>
#include <errno.h>

#define sigset_t hostcompat_sigset_t
typedef unsigned long hostcompat_sigset_t;

#define sigaction hostcompat_sigaction
struct hostcompat_sigaction {
	void (*sa_handler)(int);
	hostcompat_sigset_t sa_mask;
	int sa_flags;
};

#define SA_RESTART 0x10000000
#define SIG_BLOCK 0
#define SIG_UNBLOCK 1
#define SIG_SETMASK 2

static inline int sigemptyset(hostcompat_sigset_t *s) { *s = 0; return 0; }
static inline int sigaddset(hostcompat_sigset_t *s, int n) { *s |= 1ul << n; return 0; }
static inline int sigismember(const hostcompat_sigset_t *s, int n) { return (int)((*s >> n) & 1); }

static inline int sigprocmask(int how, const hostcompat_sigset_t *set, hostcompat_sigset_t *old)
{
	(void)how; (void)set;
	if (old)
		*old = 0;
	return 0;
}

static inline int hostcompat_sigaction(int n, const struct hostcompat_sigaction *sa,
				       struct hostcompat_sigaction *old)
{
	(void)n; (void)sa; (void)old;
	errno = ENOSYS;
	return -1;
}

static inline int kill(int pid, int sig)
{
	(void)pid; (void)sig;
	errno = ENOSYS;
	return -1;
}
#endif
