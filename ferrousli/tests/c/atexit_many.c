/*
 * Exit handlers beyond the 32 POSIX requires: a hundred, run newest first,
 * with one registered while exit is already running them, which must run
 * next. Prints "ok" from the first handler registered, which runs last.
 */

#include <stdint.h>
#include <stdlib.h>
#include <unistd.h>

int __cxa_atexit(void (*func)(void *), void *arg, void *dso);

#define COUNT 100

static intptr_t next = COUNT;
static int wrong;
static int late_ran;

static void late(void *arg)
{
	(void)arg;
	/* Handler 50 registered this and has already run. */
	if (next != 49)
		wrong = 1;
	late_ran = 1;
}

static void numbered(void *arg)
{
	if ((intptr_t)arg != next)
		wrong = 1;
	next--;
	if ((intptr_t)arg == 50 && __cxa_atexit(late, 0, 0) != 0)
		wrong = 1;
}

static void verdict(void)
{
	if (wrong || next != 0 || !late_ran)
		write(1, "bad\n", 4);
	else
		write(1, "ok\n", 3);
}

int main(void)
{
	if (atexit(verdict))
		return 1;
	for (intptr_t i = 1; i <= COUNT; i++)
		if (__cxa_atexit(numbered, (void *)i, 0))
			return 2;
	return 0;
}
