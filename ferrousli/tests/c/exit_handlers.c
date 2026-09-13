/*
 * exit runs the atexit handlers newest first, then the destructors, then
 * ends the process with the status it was given.
 */

#include <stdio.h>
#include <stdlib.h>

static void first(void)
{
	puts("first");
}

static void second(void)
{
	puts("second");
}

__attribute__((destructor))
static void destructor(void)
{
	puts("destructor");
}

int main(void)
{
	if (atexit(first) || atexit(second))
		return 1;
	puts("main");
	exit(7);
}
