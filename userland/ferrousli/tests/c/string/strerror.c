/*
 * strerror, both names of the XSI strerror_r, and strsignal.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <string.h>
#include "check.h"

/* glibc's name for the XSI strerror_r, which musl's headers do not declare. */
int __xpg_strerror_r(int, char *, size_t);

int main(void)
{
	char buf[64];

	CHECK(!strcmp(strerror(ENOENT), "No such file or directory"));
	CHECK(!strcmp(strerror(EAGAIN), "Resource temporarily unavailable"));
	CHECK(strerror(EWOULDBLOCK) == strerror(EAGAIN));
	CHECK(!strcmp(strerror(EOPNOTSUPP), "Not supported"));
	CHECK(!strcmp(strerror(ECHRNG), "Channel number out of range"));
	CHECK(!strcmp(strerror(EHWPOISON), "Memory page has hardware error"));
	CHECK(!strcmp(strerror(0), "No error information"));
	CHECK(!strcmp(strerror(-1), "No error information"));
	CHECK(!strcmp(strerror(100000), "No error information"));
	for (int e = 1; e < 134; e++)
		if (e != 41 && e != 58)
			CHECK(strcmp(strerror(e), "No error information") != 0);

	errno = 0;
	CHECK(strerror_r(EDOM, buf, sizeof buf) == 0);
	CHECK(!strcmp(buf, "Domain error"));
	CHECK(strerror_r(EDOM, buf, 13) == 0);
	CHECK(!strcmp(buf, "Domain error"));
	CHECK(strerror_r(EDOM, buf, 12) == ERANGE);
	CHECK(!strcmp(buf, "Domain erro"));
	buf[0] = 'x';
	CHECK(strerror_r(EDOM, buf, 0) == ERANGE);
	CHECK(buf[0] == 'x');
	CHECK(__xpg_strerror_r(EPERM, buf, sizeof buf) == 0);
	CHECK(!strcmp(buf, "Operation not permitted"));
	CHECK(__xpg_strerror_r(EPERM, buf, 5) == ERANGE);
	CHECK(!strcmp(buf, "Oper"));
	/* It reports through its result, not errno. */
	CHECK(errno == 0);

	CHECK(!strcmp(strsignal(SIGHUP), "Hangup"));
	CHECK(!strcmp(strsignal(SIGSEGV), "Segmentation fault"));
	CHECK(!strcmp(strsignal(SIGSTKFLT), "Stack fault"));
	CHECK(!strcmp(strsignal(SIGSYS), "Bad system call"));
	CHECK(!strcmp(strsignal(32), "RT32"));
	CHECK(!strcmp(strsignal(64), "RT64"));
	CHECK(!strcmp(strsignal(0), "Unknown signal"));
	CHECK(!strcmp(strsignal(65), "Unknown signal"));
	CHECK(!strcmp(strsignal(-1), "Unknown signal"));

	return t_status;
}
