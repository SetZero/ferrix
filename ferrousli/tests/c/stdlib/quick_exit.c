/* quick_exit runs only its own handlers, newest first, without flushing. */

#include <stdlib.h>
#include <unistd.h>

static void nothing(void) {}
static void normal(void) { write(1, "normal\n", 7); }
static void first(void) { write(1, "first\n", 6); }
static void second(void) { write(1, "second\n", 7); }

__attribute__((destructor))
static void destructor(void)
{
	write(1, "destructor\n", 11);
}

int main(void)
{
	if (atexit(normal))
		return 1;
	for (int i = 0; i < 30; i++)
		if (at_quick_exit(nothing))
			return 2;
	if (at_quick_exit(first) || at_quick_exit(second))
		return 3;
	/* ISO C requires at least 32 slots; this implementation has exactly 32. */
	if (at_quick_exit(nothing) == 0)
		return 4;
	quick_exit(23);
}
