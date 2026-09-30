/* For mconf, which the busybox build links but never runs. */
#ifndef HOSTCOMPAT_SYS_IOCTL_H
#define HOSTCOMPAT_SYS_IOCTL_H
#include <errno.h>

struct winsize {
	unsigned short ws_row, ws_col, ws_xpixel, ws_ypixel;
};
#define TIOCGWINSZ 0x5413

static inline int ioctl(int fd, unsigned long request, ...)
{
	(void)fd; (void)request;
	errno = ENOSYS;
	return -1;
}
#endif
