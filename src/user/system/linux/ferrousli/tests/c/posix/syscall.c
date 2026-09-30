/*
 * syscall passes long arguments unchanged and returns -1 with errno.
 *
 * The first check is adapted from libc-test's
 * src/regression/syscall-sign-extend.c (MIT), which checks that a pointer is
 * not sign-extended.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	char buf[1] = { 1 };
	void *p;
	long r;
	int fd;

	CHECK((fd = open("/dev/zero", O_RDONLY)) >= 0);
	CHECK((r = syscall(SYS_read, fd, buf, 1)) == 1);
	CHECK(buf[0] == 0);

	/* No arguments, a failure, and all six. */
	CHECK(syscall(SYS_getpid) == getpid());
	errno = 0;
	CHECK(syscall(SYS_close, -1) == -1 && errno == EBADF);
#ifdef SYS_mmap2
	/* 32-bit Arm has no mmap, only mmap2, whose offset is in pages. */
	p = (void *)syscall(SYS_mmap2, 0, 4096, PROT_READ | PROT_WRITE,
			    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
#else
	p = (void *)syscall(SYS_mmap, 0, 4096, PROT_READ | PROT_WRITE,
			    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
#endif
	CHECK(p != MAP_FAILED);
	CHECK(syscall(SYS_munmap, p, 4096) == 0);
	CHECK(close(fd) == 0);

	return t_status;
}
