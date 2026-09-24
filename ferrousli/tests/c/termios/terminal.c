/*
 * termios.h and unistd.h's terminal calls, on a pseudo-terminal the test
 * opens itself with stdlib.h's posix_openpt, grantpt, unlockpt and ptsname:
 * attributes read, set with each action and read back, the speed calls, raw
 * mode against the header's own flags, draining, flushing and flow, window
 * sizes, ttyname, and the process group calls, which fail on a terminal that
 * controls no session.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <unistd.h>

#include "check.h"

static void append_number(char *s, int n)
{
	char digits[12];
	int i = 0;

	do {
		digits[i++] = '0' + n % 10;
		n /= 10;
	} while (n);
	s += strlen(s);
	while (i)
		*s++ = digits[--i];
	*s = 0;
}

int main(void)
{
	struct termios t, u, raw;
	struct winsize size = { 24, 80, 0, 0 }, got;
	char name[64], path[32] = "/dev/pts/";
	int master, slave, null, number = -1;
	const tcflag_t iflags = IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON;
	const tcflag_t lflags = ECHO | ECHONL | ICANON | ISIG | IEXTEN;

	/* The pair, through stdlib.h's calls, and the slave's name both ways. */
	master = posix_openpt(O_RDWR | O_NOCTTY);
	CHECK(master >= 0);
	CHECK(grantpt(master) == 0 && unlockpt(master) == 0);
	CHECK(ioctl(master, TIOCGPTN, &number) == 0);
	append_number(path, number);
	CHECK(ptsname(master) && !strcmp(ptsname(master), path));
	CHECK(ptsname_r(master, name, sizeof name) == 0 && !strcmp(name, path));
	CHECK(ptsname_r(master, name, strlen(path)) == ERANGE);
	slave = open(path, O_RDWR | O_NOCTTY);
	CHECK(slave >= 0);
	null = open("/dev/null", O_RDONLY);
	errno = 0;
	CHECK(grantpt(null) == -1 && errno == EINVAL);
	CHECK(ptsname_r(null, name, sizeof name) == ENOTTY);
	errno = 0;
	CHECK(ptsname(null) == 0 && errno == ENOTTY);

	/* Attributes: read, changed, set with each action, and read back. */
	CHECK(tcgetattr(slave, &t) == 0);
	u = t;
	u.c_lflag &= ~ECHO;
	CHECK(tcsetattr(slave, TCSANOW, &u) == 0);
	CHECK(tcgetattr(slave, &u) == 0 && !(u.c_lflag & ECHO));
	CHECK(tcsetattr(slave, TCSADRAIN, &t) == 0 && tcsetattr(slave, TCSAFLUSH, &t) == 0);
	CHECK(tcgetattr(slave, &u) == 0 && (u.c_lflag & ECHO) == (t.c_lflag & ECHO));
	errno = 0;
	CHECK(tcsetattr(slave, 3, &t) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(tcgetattr(null, &u) == -1 && errno == ENOTTY);

	/* The speeds, which are c_cflag's CBAUD bits. */
	u = t;
	CHECK(cfsetospeed(&u, B9600) == 0 && cfgetospeed(&u) == B9600 && cfgetispeed(&u) == B9600);
	CHECK(cfsetispeed(&u, 0) == 0 && cfgetispeed(&u) == B9600);
	CHECK(cfsetispeed(&u, B115200) == 0 && cfgetospeed(&u) == B115200);
	CHECK(cfsetspeed(&u, B38400) == 0 && cfgetospeed(&u) == B38400);
	CHECK((u.c_cflag & ~CBAUD) == (t.c_cflag & ~CBAUD));
	errno = 0;
	CHECK(cfsetospeed(&u, 0x7fffffff) == -1 && errno == EINVAL);

	/* Raw mode clears exactly these flags, as the header spells them. */
	memset(&raw, 0xff, sizeof raw);
	cfmakeraw(&raw);
	CHECK(raw.c_iflag == (tcflag_t)~iflags);
	CHECK(raw.c_oflag == (tcflag_t)~OPOST);
	CHECK(raw.c_lflag == (tcflag_t)~lflags);
	CHECK((raw.c_cflag & (CSIZE | PARENB)) == CS8);
	CHECK(raw.c_cc[VMIN] == 1 && raw.c_cc[VTIME] == 0);

	/* Output drained and flushed, flow stopped and restarted, a break. */
	CHECK(write(slave, "x", 1) == 1);
	CHECK(tcdrain(slave) == 0);
	CHECK(tcflush(slave, TCIOFLUSH) == 0);
	CHECK(tcflow(slave, TCOOFF) == 0 && tcflow(slave, TCOON) == 0);
	CHECK(tcsendbreak(slave, 0) == 0);

	/* A window size set on the master is read on the slave. */
	CHECK(tcsetwinsize(master, &size) == 0 && tcgetwinsize(slave, &got) == 0);
	CHECK(got.ws_row == 24 && got.ws_col == 80);

	/* ttyname names the slave, and refuses a buffer too small for the name
	 * and its NUL, and a file that is not a terminal. */
	CHECK(ttyname_r(slave, name, sizeof name) == 0 && !strcmp(name, path));
	CHECK(ttyname(slave) && !strcmp(ttyname(slave), path));
	CHECK(ttyname_r(slave, name, strlen(path)) == ERANGE);
	CHECK(ttyname_r(null, name, sizeof name) == ENOTTY);
	errno = 0;
	CHECK(!ttyname(null) && errno == ENOTTY);

	/* The terminal controls no session. */
	errno = 0;
	CHECK(tcgetpgrp(slave) == -1 && errno == ENOTTY);
	errno = 0;
	CHECK(tcsetpgrp(slave, getpgrp()) == -1 && errno == ENOTTY);
	errno = 0;
	CHECK(tcgetsid(slave) == -1 && errno == ENOTTY);

	close(slave);
	close(master);
	close(null);
	return t_status;
}
