/*
 * Raw system calls, so that a test can make files and directories without
 * depending on the library's wrappers. The numbers come from the headers the
 * program is built against.
 */

#ifndef FERROUSLI_TEST_SYS_H
#define FERROUSLI_TEST_SYS_H

#include <fcntl.h>
#include <sys/syscall.h>

long t_syscall(long n, long a, long b, long c, long d);

/* x86-64: the number in rax, the arguments in rdi, rsi, rdx and r10. */
__asm__(
	".text\n"
	".globl t_syscall\n"
	".type t_syscall, @function\n"
	"t_syscall:\n"
	"	mov %rdi, %rax\n"
	"	mov %rsi, %rdi\n"
	"	mov %rdx, %rsi\n"
	"	mov %rcx, %rdx\n"
	"	mov %r8, %r10\n"
	"	syscall\n"
	"	ret\n"
);

__attribute__((unused))
static int t_mkfile(const char *name)
{
	long fd = t_syscall(SYS_openat, AT_FDCWD, (long)name,
		O_CREAT | O_WRONLY | O_TRUNC, 0644);
	if (fd < 0)
		return -1;
	t_syscall(SYS_close, fd, 0, 0, 0);
	return 0;
}

__attribute__((unused))
static int t_mkdir(const char *name)
{
	return t_syscall(SYS_mkdirat, AT_FDCWD, (long)name, 0755, 0) < 0 ? -1 : 0;
}

__attribute__((unused))
static int t_symlink(const char *target, const char *name)
{
	return t_syscall(SYS_symlinkat, (long)target, AT_FDCWD, (long)name, 0) < 0 ? -1 : 0;
}

__attribute__((unused))
static int t_open(const char *name, int flags)
{
	return (int)t_syscall(SYS_openat, AT_FDCWD, (long)name, flags, 0);
}

__attribute__((unused))
static void t_close(int fd)
{
	t_syscall(SYS_close, fd, 0, 0, 0);
}

#endif
