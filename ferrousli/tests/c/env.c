/*
 * setenv, unsetenv, putenv and clearenv. The harness starts the program with
 * one variable, KEEP=1.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

extern char **environ;

static int count(void)
{
	int n = 0;
	for (char **e = environ; e && *e; e++)
		n++;
	return n;
}

int main(void)
{
	CHECK(count() == 1);
	CHECK(strcmp(getenv("KEEP"), "1") == 0);

	/* setenv adds, keeps or replaces. */
	CHECK(setenv("NEW", "value", 0) == 0);
	CHECK(strcmp(getenv("NEW"), "value") == 0);
	CHECK(setenv("NEW", "ignored", 0) == 0);
	CHECK(strcmp(getenv("NEW"), "value") == 0);
	CHECK(setenv("NEW", "replaced", 1) == 0);
	CHECK(strcmp(getenv("NEW"), "replaced") == 0);
	CHECK(setenv("KEEP", "2", 1) == 0);
	CHECK(strcmp(getenv("KEEP"), "2") == 0);
	CHECK(count() == 2);

	/* Names that cannot be variables. */
	errno = 0;
	CHECK(setenv("", "x", 1) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(setenv("A=B", "x", 1) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(unsetenv("") == -1 && errno == EINVAL);
	errno = 0;
	CHECK(unsetenv("A=") == -1 && errno == EINVAL);

	/* putenv puts the program's own string in the environment. */
	static char mine[] = "MINE=before";
	CHECK(putenv(mine) == 0);
	CHECK(strcmp(getenv("MINE"), "before") == 0);
	memcpy(mine + 5, "after!", 7);
	CHECK(strcmp(getenv("MINE"), "after!") == 0);
	CHECK(count() == 3);
	/* A name with no '=' removes the variable. */
	static char bare[] = "MINE";
	CHECK(putenv(bare) == 0);
	CHECK(getenv("MINE") == 0);
	CHECK(count() == 2);

	/* unsetenv removes every copy of a name, from the program's own array. */
	static char a[] = "TWICE=1", b[] = "OTHER=x", c[] = "TWICE=2";
	static char *own[] = { a, b, c, 0 };
	environ = own;
	CHECK(unsetenv("TWICE") == 0);
	CHECK(getenv("TWICE") == 0);
	CHECK(count() == 1);
	CHECK(own[0] == b && own[1] == 0);
	/* Adding to the program's array copies it rather than resizing it. */
	CHECK(setenv("AFTER", "own", 1) == 0);
	CHECK(environ != own);
	CHECK(count() == 2);
	CHECK(strcmp(getenv("OTHER"), "x") == 0);

	/* clearenv empties the environment, which can then be filled again. */
	CHECK(clearenv() == 0);
	CHECK(count() == 0);
	CHECK(getenv("OTHER") == 0);
	CHECK(setenv("AGAIN", "yes", 0) == 0);
	CHECK(count() == 1);

	/* Growth, and replacing one value many times. */
	for (int i = 0; i < 500; i++) {
		char name[] = { 'V', '0' + i / 100, '0' + i / 10 % 10, '0' + i % 10, 0 };
		if (setenv(name, name, 1) != 0) {
			CHECK(!"setenv could grow the environment");
			break;
		}
	}
	CHECK(count() == 501);
	CHECK(strcmp(getenv("V250"), "V250") == 0);
	for (int i = 0; i < 10000; i++) {
		if (setenv("AGAIN", (i & 1) ? "odd" : "even", 1) != 0) {
			CHECK(!"setenv could replace a value");
			break;
		}
	}
	CHECK(strcmp(getenv("AGAIN"), "odd") == 0);
	CHECK(count() == 501);

	return t_status;
}
