/*
 * Context for a failed CHECK in the time tests, written with write() since
 * there is no stdio yet: say() writes a string and say_number() a number to
 * standard error.
 */

#ifndef FERROUSLI_TEST_TIME_REPORT_H
#define FERROUSLI_TEST_TIME_REPORT_H

#include <string.h>
#include <unistd.h>

__attribute__((unused))
static void say(const char *s)
{
	write(2, s, strlen(s));
}

__attribute__((unused))
static void say_number(long long n)
{
	char buf[24];
	int i = sizeof buf;
	unsigned long long u = n < 0 ? -(unsigned long long)n : (unsigned long long)n;
	do {
		buf[--i] = '0' + u % 10;
		u /= 10;
	} while (u);
	if (n < 0)
		buf[--i] = '-';
	write(2, buf + i, sizeof buf - i);
}

#endif
