/*
 * Processes: fork and the wait family with the status decoded, a child
 * killed by a fault, exec of this program by execve, execv and execvp, and
 * sessions and process groups.
 *
 * Run as "processes exec N", the program is the exec'd child: it checks its
 * environment and exits with 40 + N.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

/* The status of child `pid`, which must be the one that changed state. */
static int reap(pid_t pid)
{
	int status = -1;
	if (waitpid(pid, &status, 0) != pid)
		return -1;
	return status;
}

__attribute__((unused))
static size_t copy(char *to, const char *from)
{
	size_t n = strlen(from);
	memcpy(to, from, n + 1);
	return n;
}

int main(int argc, char **argv)
{
	char *args[4], *env[3], name[300];
	/* Unused under an emulator, which cannot run this program again. */
	__attribute__((unused)) char path[4200];
	__attribute__((unused)) const char *base;
	pid_t parent = getpid(), pid;
	struct rusage ru;
	siginfo_t si;
	int status, pfd[2], fd;
	__attribute__((unused)) size_t n;

	if (argc == 3 && strcmp(argv[1], "exec") == 0) {
		const char *value = getenv("FERROUSLI_EXEC");
		if (!value || strcmp(value, argv[2]) != 0)
			return 99;
		return 40 + argv[2][0] - '0';
	}

	/* fork, then waitpid with the exit status decoded. */
	pid = fork();
	if (pid == 0) {
		if (getppid() != parent || getpid() == parent)
			_exit(1);
#if defined(__x86_64__)
		/* The child's control block holds its own thread id, at 0x84 from
		   the thread pointer, where x86-64's layout puts the block. */
		if (((int *)__builtin_thread_pointer())[0x84 / sizeof(int)] != syscall(SYS_gettid))
			_exit(2);
#endif
		_exit(7);
	}
	CHECK(pid > 0 && pid != parent);
	status = reap(pid);
	CHECK(WIFEXITED(status) && !WIFSIGNALED(status) && WEXITSTATUS(status) == 7);
	errno = 0;
	CHECK(waitpid(pid, &status, 0) == -1 && errno == ECHILD);
	errno = 0;
	CHECK(wait(&status) == -1 && errno == ECHILD);

	/* A child that returns from main exits through exit. */
	pid = fork();
	if (pid == 0)
		return 9;
	status = reap(pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 9);

	/* A child killed by a fault: a write after mprotect(PROT_READ). */
	pid = fork();
	if (pid == 0) {
		struct rlimit none = { 0, 0 };
		volatile char *page;
		setrlimit(RLIMIT_CORE, &none);
		page = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (page == MAP_FAILED)
			_exit(1);
		page[0] = 1;
		if (mprotect((void *)page, 4096, PROT_READ) != 0)
			_exit(2);
		page[0] = 2;
		_exit(3);
	}
	status = reap(pid);
	CHECK(WIFSIGNALED(status) && !WIFEXITED(status) && WTERMSIG(status) == SIGSEGV);

	/* WNOHANG while the child runs, then wait4 with its usage. */
	CHECK(pipe(pfd) == 0);
	pid = fork();
	if (pid == 0) {
		char c;
		close(pfd[1]);
		_exit(read(pfd[0], &c, 1) == 0 ? 3 : 1);
	}
	CHECK(close(pfd[0]) == 0);
	CHECK(waitpid(pid, &status, WNOHANG) == 0);
	CHECK(close(pfd[1]) == 0);
	memset(&ru, 0xff, sizeof ru);
	CHECK(wait4(pid, &status, 0, &ru) == pid && WEXITSTATUS(status) == 3);
	CHECK(ru.ru_utime.tv_usec >= 0 && ru.ru_utime.tv_usec < 1000000);

	/* wait3, wait and waitid. */
	pid = fork();
	if (pid == 0)
		_exit(4);
	CHECK(wait3(&status, 0, 0) == pid && WEXITSTATUS(status) == 4);
	pid = fork();
	if (pid == 0)
		_exit(5);
	CHECK(wait(&status) == pid && WEXITSTATUS(status) == 5);
	pid = fork();
	if (pid == 0)
		_exit(6);
	memset(&si, 0, sizeof si);
	CHECK(waitid(P_PID, pid, &si, WEXITED) == 0);
	CHECK(si.si_pid == pid && si.si_signo == SIGCHLD && si.si_code == CLD_EXITED && si.si_status == 6);
	errno = 0;
	CHECK(waitid(P_ALL, 0, &si, WEXITED) == -1 && errno == ECHILD);
	errno = 0;
	CHECK(waitid(P_PID, pid, &si, 0) == -1 && errno == EINVAL);

	/* vfork. */
	pid = vfork();
	if (pid == 0)
		_exit(8);
	status = reap(pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 8);

#ifndef FERROUSLI_TEST_EMULATED
	/* This program running itself: qemu-user without binfmt_misc hands an
	   execve to the host kernel, which cannot run a foreign program. */
	/* execve of this program, with an environment of its own. */
	pid = fork();
	if (pid == 0) {
		args[0] = argv[0];
		args[1] = "exec";
		args[2] = "2";
		args[3] = 0;
		env[0] = "FERROUSLI_EXEC=2";
		env[1] = 0;
		execve(argv[0], args, env);
		_exit(98);
	}
	status = reap(pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 42);
#endif
	args[0] = "missing";
	args[1] = 0;
	errno = 0;
	CHECK(execve("missing", args, args + 1) == -1 && errno == ENOENT);

#ifndef FERROUSLI_TEST_EMULATED
	/* execv passes environ. */
	pid = fork();
	if (pid == 0) {
		args[0] = argv[0];
		args[1] = "exec";
		args[2] = "3";
		args[3] = 0;
		env[0] = "FERROUSLI_EXEC=3";
		env[1] = 0;
		environ = env;
		execv(argv[0], args);
		_exit(98);
	}
	status = reap(pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 43);

	/*
	 * execvp searches PATH: past a directory that does not exist, an empty
	 * entry (the working directory, which has no such program), and a file
	 * used as a directory, to the directory holding this program.
	 */
	base = argv[0];
	for (const char *s = argv[0]; *s; s++)
		if (*s == '/')
			base = s + 1;
	n = copy(path, "PATH=/nonexistent::/proc/self/exe:");
	memcpy(path + n, argv[0], base - argv[0]);
	path[n + (base - argv[0])] = 0;
	pid = fork();
	if (pid == 0) {
		args[0] = (char *)base;
		args[1] = "exec";
		args[2] = "4";
		args[3] = 0;
		env[0] = path;
		env[1] = "FERROUSLI_EXEC=4";
		env[2] = 0;
		environ = env;
		execvp(base, args);
		_exit(98);
	}
	status = reap(pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 44);

	/* A name with a slash is not searched for. */
	pid = fork();
	if (pid == 0) {
		args[0] = argv[0];
		args[1] = "exec";
		args[2] = "5";
		args[3] = 0;
		env[0] = "PATH=/nonexistent";
		env[1] = "FERROUSLI_EXEC=5";
		env[2] = 0;
		environ = env;
		execvp(argv[0], args);
		_exit(98);
	}
	status = reap(pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 45);
#endif

	/* execvp's failures, with environ swapped in the parent and back. */
	{
		char **saved = environ;
		env[0] = "PATH=/nonexistent:";
		env[1] = 0;
		environ = env;
		args[0] = "x";
		args[1] = 0;
		errno = 0;
		CHECK(execvp("", args) == -1 && errno == ENOENT);
		errno = 0;
		CHECK(execvp("no-such-program", args) == -1 && errno == ENOENT);
		fd = open("not-executable", O_WRONLY | O_CREAT | O_EXCL, 0644);
		CHECK(fd >= 0 && close(fd) == 0);
		errno = 0;
		CHECK(execvp("not-executable", args) == -1 && errno == EACCES);
		memset(name, 'n', 256);
		name[256] = 0;
		errno = 0;
		CHECK(execvp(name, args) == -1 && errno == ENAMETOOLONG);
		environ = saved;
	}

	/* Process groups and sessions. */
	CHECK(getpgrp() == getpgid(0) && getpgid(getpid()) == getpgrp());
	CHECK(getsid(0) > 0 && getsid(getpid()) == getsid(0));
	errno = 0;
	CHECK(getpgid(-1) == -1 && errno == ESRCH);
	pid = fork();
	if (pid == 0) {
		if (setpgid(0, 0) != 0 || getpgrp() != getpid())
			_exit(1);
		errno = 0;
		/* A group leader cannot start a session. */
		if (setsid() != -1 || errno != EPERM)
			_exit(2);
		_exit(0);
	}
	status = reap(pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
	pid = fork();
	if (pid == 0) {
		if (setsid() != getpid())
			_exit(1);
		if (getsid(0) != getpid() || getpgrp() != getpid())
			_exit(2);
		_exit(0);
	}
	status = reap(pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);

	return t_status;
}
