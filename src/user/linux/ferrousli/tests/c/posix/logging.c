/*
 * utmpx.h's records, which are never kept, and syslog.h's calls. Every
 * message is below the mask or has a priority out of range, so the test
 * sends nothing to the system's logger.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdarg.h>
#include <string.h>
#include <syslog.h>
#include <utmpx.h>

#include "check.h"

static void vlog(int priority, const char *format, ...)
{
	va_list ap;

	va_start(ap, format);
	vsyslog(priority, format, ap);
	va_end(ap);
}

int main(void)
{
	struct utmpx ut;

	/* No records, and nothing stored. */
	memset(&ut, 0, sizeof ut);
	ut.ut_type = USER_PROCESS;
	setutxent();
	CHECK(!getutxent() && !getutxid(&ut) && !getutxline(&ut));
	CHECK(!pututxline(&ut));
	updwtmpx("wtmp", &ut);
	endutxent();
	errno = 0;
	CHECK(utmpxname("utmp") == -1 && errno == ENOTSUP);

	/* The mask starts with every priority, and zero reads it. */
	openlog("ferrousli-test", LOG_PID, LOG_LOCAL0);
	CHECK(setlogmask(0) == 0xff);
	CHECK(setlogmask(LOG_MASK(LOG_EMERG)) == 0xff);
	CHECK(setlogmask(0) == LOG_MASK(LOG_EMERG));

	/* Masked out, or out of range: neither is formatted nor sent. */
	syslog(LOG_DEBUG, "masked %s", "out");
	vlog(LOG_INFO, "masked %d", 1);
	syslog(LOG_EMERG | 0x400, "an out-of-range priority");
	CHECK(setlogmask(0xff) == LOG_MASK(LOG_EMERG));
	closelog();
	closelog();
	return t_status;
}
