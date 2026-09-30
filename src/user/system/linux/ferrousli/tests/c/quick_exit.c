/*
 * quick_exit runs the at_quick_exit handlers newest first, including one a
 * handler registers while they run, and nothing else: no atexit handler, and
 * no stdio flush. The 33rd registration fails, as C's 32 slots and musl's are
 * all there is. Output goes through write, so a flush cannot fake it.
 */

#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void say(const char *s)
{
	(void)!write(1, s, strlen(s));
}

static void never(void)
{
	say("an atexit handler ran\n");
}

static void nothing(void)
{
}

static void late(void)
{
	say("late\n");
}

static void first(void)
{
	say("first\n");
}

static void second(void)
{
	say("second\n");
	if (at_quick_exit(late) != 0)
		say("registering during quick_exit failed\n");
}

int main(void)
{
	if (atexit(never) != 0)
		return 1;
	if (at_quick_exit(first) != 0 || at_quick_exit(second) != 0)
		return 2;
	for (int i = 0; i < 30; i++)
		if (at_quick_exit(nothing) != 0)
			return 3;
	if (at_quick_exit(nothing) != -1)
		return 4;
	if (at_quick_exit(NULL) != -1)
		return 5;
	say("main\n");
	quick_exit(7);
}
