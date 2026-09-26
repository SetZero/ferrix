/*
 * Everything `__libc_start_main` sets up reaches `main`: the arguments, the
 * environment, and constructors that ran first.
 *
 * Prints each argument after the program's name, then FERROUSLI_TEST's value,
 * and exits with argc. Any other status names the check that failed.
 */

#include <stdio.h>
#include <stdlib.h>

static int constructed;

__attribute__((constructor))
static void construct(void)
{
	constructed = 1;
}

int main(int argc, char **argv)
{
	if (!constructed)
		return 10;
	if (argv[argc] != 0)
		return 11;

	for (int i = 1; i < argc; i++)
		puts(argv[i]);

	const char *value = getenv("FERROUSLI_TEST");
	puts(value ? value : "(unset)");

	/* A prefix of a variable's name is not that variable. */
	if (getenv("FERROUSLI") != 0)
		return 12;
	return argc;
}
