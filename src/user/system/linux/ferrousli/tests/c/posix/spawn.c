/*
 * Running programs: popen reading a command's output and writing its input,
 * pclose's status, a popen child that must not hold an earlier popen's pipe,
 * system's status, its null command and a shell killed by SIGINT, the execl
 * family with a list, an environment and a search, and daemon detaching
 * into a session of its own.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

static int run(int which)
{
	char *envp[] = { "FERROUSLI=yes", 0 };
	pid_t pid = fork();
	int status;

	if (pid == 0) {
		if (which == 0)
			execl("/bin/sh", "sh", "-c", "exit 4", (char *)0);
		else if (which == 1)
			execle("/bin/sh", "sh", "-c", "test \"$FERROUSLI\" = yes && exit 5", (char *)0, envp);
		else
			execlp("sh", "sh", "-c", "exit 6", (char *)0);
		_exit(127);
	}
	if (pid < 0 || waitpid(pid, &status, 0) != pid || !WIFEXITED(status))
		return -1;
	return WEXITSTATUS(status);
}

int main(void)
{
	FILE *in, *out, *hold;
	char buf[64], c = 0;
	int status, fd, i;
	pid_t sid = getsid(0), pid;

	/* A command's output, and its exit status. */
	in = popen("echo hello; exit 3", "r");
	CHECK(in != 0);
	if (in) {
		CHECK(fgets(buf, sizeof buf, in) && !strcmp(buf, "hello\n"));
		status = pclose(in);
		CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 3);
	}

	/* A command's input, while a second popen stream is open. The second
	 * child must close the first's pipe, or cat never sees its end and
	 * pclose never returns. */
	out = popen("cat > written", "w");
	hold = popen("cat > /dev/null", "we");
	CHECK(out && hold);
	if (out && hold) {
		CHECK(!(fcntl(fileno(out), F_GETFD) & FD_CLOEXEC));
		CHECK(fcntl(fileno(hold), F_GETFD) & FD_CLOEXEC);
		CHECK(fputs("through a pipe\n", out) >= 0);
		CHECK(pclose(out) == 0);
		fd = open("written", O_RDONLY);
		CHECK(fd >= 0 && read(fd, buf, sizeof buf) == 15 && !memcmp(buf, "through a pipe\n", 15));
		close(fd);
		CHECK(pclose(hold) == 0);
	}
	errno = 0;
	CHECK(!popen("true", "x") && errno == EINVAL);

	/* system. */
	CHECK(system(0) != 0);
	status = system("exit 7");
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 7);
	status = system("kill -INT $$");
	CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGINT);

	/* The execl family. */
	CHECK(run(0) == 4);
	CHECK(run(1) == 5);
	CHECK(run(2) == 6);

	/* daemon: the final child is in a new session it does not lead, reads
	 * the end of file on standard input, and says so in a file. */
	pid = fork();
	if (pid == 0) {
		if (daemon(1, 0) == 0) {
			char answer = getsid(0) != sid && getsid(0) != getpid() && read(0, buf, 1) == 0 ? 'y' : 'n';
			fd = open("daemon.out", O_WRONLY | O_CREAT, 0600);
			if (fd >= 0 && write(fd, &answer, 1) == 1)
				close(fd);
		}
		_exit(0);
	}
	CHECK(pid > 0 && waitpid(pid, &status, 0) == pid && WIFEXITED(status));
	for (i = 0; i < 500 && !c; i++) {
		fd = open("daemon.out", O_RDONLY);
		if (fd >= 0) {
			if (read(fd, &c, 1) != 1)
				c = 0;
			close(fd);
		}
		if (!c)
			usleep(10000);
	}
	CHECK(c == 'y');
	return t_status;
}
