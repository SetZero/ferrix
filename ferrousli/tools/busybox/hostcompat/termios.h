/* For mconf, which the busybox build links but never runs. */
#ifndef HOSTCOMPAT_TERMIOS_H
#define HOSTCOMPAT_TERMIOS_H
#include <errno.h>

struct termios {
	unsigned c_iflag, c_oflag, c_cflag, c_lflag;
	unsigned char c_cc[32];
};
#define TCSANOW 0
#define TCSADRAIN 1
#define TCSAFLUSH 2

static inline int tcgetattr(int fd, struct termios *t)
{
	(void)fd; (void)t;
	errno = ENOSYS;
	return -1;
}

static inline int tcsetattr(int fd, int action, const struct termios *t)
{
	(void)fd; (void)action; (void)t;
	errno = ENOSYS;
	return -1;
}
#endif
