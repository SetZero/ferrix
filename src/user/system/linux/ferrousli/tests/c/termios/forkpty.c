/*
 * pty.h's openpty and forkpty and utmp.h's login_tty: a pair opened with its
 * slave's name, attributes and size; a child forked onto a new terminal,
 * which is its controlling terminal and its three standard streams in a
 * session of its own; and login_tty refusing a file that is no terminal.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pty.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>
#include <utmp.h>

#include "check.h"

/* What the child finds, one letter a check, written to its terminal. */
static void child(const struct winsize *want)
{
	struct winsize got;
	char report[7] = ".....\n";

	report[0] = isatty(0) ? 'a' : '0';
	report[1] = isatty(1) && isatty(2) ? 'b' : '1';
	report[2] = getsid(0) == getpid() ? 'c' : '2';
	report[3] = tcgetpgrp(0) == getpgrp() ? 'd' : '3';
	report[4] = ioctl(0, TIOCGWINSZ, &got) == 0 && got.ws_row == want->ws_row
		&& got.ws_col == want->ws_col ? 'e' : '4';
	write(1, report, sizeof report - 1);
	_exit(0);
}

int main(void)
{
	struct winsize size = { 31, 97, 0, 0 }, got;
	struct termios t;
	char name[20], seen[64];
	int master, slave, null, status, used = 0;
	pid_t pid;
	ssize_t n;

	/* openpty: both ends, the slave's name, and the size it was given. */
	CHECK(openpty(&master, &slave, name, 0, &size) == 0);
	CHECK(ptsname(master) && !strcmp(ptsname(master), name));
	CHECK(isatty(slave) && ioctl(slave, TIOCGWINSZ, &got) == 0);
	CHECK(got.ws_row == 31 && got.ws_col == 97);
	CHECK(tcgetattr(slave, &t) == 0);
	close(slave);
	close(master);

	/* The attributes it was given, too. */
	t.c_lflag &= ~ECHO;
	CHECK(openpty(&master, &slave, 0, &t, 0) == 0);
	CHECK(tcgetattr(slave, &t) == 0 && !(t.c_lflag & ECHO));
	close(slave);
	close(master);

	/* forkpty: the child reports on its terminal through the master. */
	null = open("/dev/null", O_RDONLY | O_CLOEXEC);
	pid = forkpty(&master, name, 0, &size);
	if (pid == 0)
		child(&size);
	CHECK(pid > 0 && master >= 0);
	CHECK(!strncmp(name, "/dev/pts/", 9));
	while (used < (int)sizeof seen - 1
		&& (n = read(master, seen + used, sizeof seen - 1 - used)) > 0) {
		used += n;
		if (memchr(seen, '\n', used))
			break;
	}
	seen[used] = 0;
	CHECK(!strncmp(seen, "abcde", 5));
	CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
	close(master);

	/* login_tty on a file that is no terminal fails, in a child, since it
	   starts a session first. */
	pid = fork();
	if (pid == 0)
		_exit(login_tty(null) == -1 && errno == ENOTTY ? 0 : 1);
	CHECK(pid > 0 && waitpid(pid, &status, 0) == pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
	close(null);
	return t_status;
}
