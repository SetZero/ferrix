/*
 * assert: a true assertion does nothing, and a false one writes musl's
 * message to standard error and ends the process with SIGABRT. With NDEBUG,
 * neither is evaluated.
 */

#include <assert.h>
#include <string.h>
#include <unistd.h>

static int evaluated;

/* Unused when NDEBUG takes the assertion away. */
__attribute__((unused))
static int count(int value)
{
	evaluated++;
	return value;
}

int main(int argc, char **argv)
{
	assert(count(argc >= 1));
#ifdef NDEBUG
	if (evaluated != 0)
		return 2;
#else
	if (evaluated != 1)
		return 2;
#endif
	if (argc > 1 && strcmp(argv[1], "fail") == 0) {
		/* The file is named without its directory, so the message does
		 * not depend on where the tree is checked out. */
#line 100 "assert.c"
		assert(1 + 1 == 3);
	}
	write(1, "passed\n", 7);
	return 0;
}
