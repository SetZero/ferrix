/*
 * The names Debian's python3.13 imports that no program before it did:
 * __sysconf, ctermid, fexecve, preadv2 and pwritev2 under their plain and
 * 64 names, process_vm_readv and process_vm_writev, and posix_spawn's
 * addclosefrom_np file action. openpty, forkpty and login_tty are
 * termios/forkpty.c's.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

extern char **environ;
long __sysconf(int);
ssize_t preadv64v2(int, const struct iovec *, int, off_t, int);
ssize_t pwritev64v2(int, const struct iovec *, int, off_t, int);

/* Runs `sh -c script` with fd 10 open and not close-on-exec, with or
   without a closefrom action from 3, and returns its exit status. */
static int spawn_with_ten(const char *script, int closefrom)
{
	posix_spawn_file_actions_t acts;
	char *argv[] = { "sh", "-c", (char *)script, 0 };
	pid_t pid;
	int status = -1;

	CHECK(posix_spawn_file_actions_init(&acts) == 0);
	if (closefrom)
		CHECK(posix_spawn_file_actions_addclosefrom_np(&acts, 3) == 0);
	CHECK(posix_spawn(&pid, "/bin/sh", &acts, 0, argv, environ) == 0);
	CHECK(waitpid(pid, &status, 0) == pid);
	posix_spawn_file_actions_destroy(&acts);
	return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

int main(void)
{
	char name[L_ctermid], path[] = "/tmp/ferrousli-python-XXXXXX";
	char a[4] = { 0 }, b[4] = { 0 }, source[6] = "remote", copy[6] = { 0 };
	struct iovec iov[2] = { { a, 3 }, { b, 3 } }, local, remote;
	int fd, status;
	pid_t pid;

	/* __sysconf is sysconf. */
	CHECK(__sysconf(_SC_PAGESIZE) == sysconf(_SC_PAGESIZE));

	/* ctermid names the controlling terminal, in the caller's buffer or
	   its own. */
	CHECK(ctermid(name) == name && !strcmp(name, "/dev/tty"));
	CHECK(!strcmp(ctermid(0), "/dev/tty"));

	/* preadv2 and pwritev2, at an offset and, with -1, at the file's. */
	fd = mkstemp(path);
	CHECK(fd >= 0);
	unlink(path);
	CHECK(write(fd, "0123456789", 10) == 10);
	CHECK(preadv2(fd, iov, 2, 2, 0) == 6);
	CHECK(!memcmp(a, "234", 3) && !memcmp(b, "567", 3));
	CHECK(lseek(fd, 1, SEEK_SET) == 1);
	CHECK(preadv64v2(fd, iov, 1, -1, 0) == 3 && !memcmp(a, "123", 3));
	CHECK(lseek(fd, 0, SEEK_CUR) == 4);
	memcpy(a, "abc", 3);
	CHECK(pwritev2(fd, iov, 1, 7, 0) == 3);
	CHECK(pwritev64v2(fd, iov, 1, 0, 0) == 3);
	CHECK(pread(fd, b, 3, 7) == 3 && !memcmp(b, "abc", 3));
	CHECK(pread(fd, b, 3, 0) == 3 && !memcmp(b, "abc", 3));
	errno = 0;
	CHECK(preadv2(fd, iov, 1, 0, 0x7fffffff) == -1 && errno == EOPNOTSUPP);
	close(fd);

	/* process_vm_readv and _writev, on this process's own memory. */
	local.iov_base = copy;
	local.iov_len = sizeof copy;
	remote.iov_base = source;
	remote.iov_len = sizeof source;
	CHECK(process_vm_readv(getpid(), &local, 1, &remote, 1, 0) == 6);
	CHECK(!memcmp(copy, "remote", 6));
	memcpy(copy, "writes", 6);
	CHECK(process_vm_writev(getpid(), &local, 1, &remote, 1, 0) == 6);
	CHECK(!memcmp(source, "writes", 6));

	/* fexecve runs the program open at a descriptor; a closed one is
	   EBADF. */
	fd = open("/bin/sh", O_RDONLY | O_CLOEXEC);
	CHECK(fd >= 0);
	pid = fork();
	if (pid == 0) {
		char *argv[] = { "sh", "-c", "exit 7", 0 };
		fexecve(fd, argv, environ);
		_exit(99);
	}
	CHECK(pid > 0 && waitpid(pid, &status, 0) == pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 7);
	close(fd);
	errno = 0;
	{
		char *argv[] = { "sh", 0 };
		CHECK(fexecve(fd, argv, environ) == -1 && errno == EBADF);
	}

	/* The closefrom action closes fd 10 in the child; without it, the
	   child has it. */
	CHECK(dup2(1, 10) == 10);
	CHECK(spawn_with_ten("test -e /proc/self/fd/10", 0) == 0);
	CHECK(spawn_with_ten("test ! -e /proc/self/fd/10", 1) == 0);
	close(10);
	return t_status;
}
