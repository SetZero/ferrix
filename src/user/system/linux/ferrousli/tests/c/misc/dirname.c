/*
 * dirname, adapted from libc-test's src/functional/dirname.c (MIT).
 */

#include <libgen.h>
#include <string.h>
#include "check.h"

#define T(path, want) do { \
	char tmp[100]; \
	CHECK(strcmp(dirname(strcpy(tmp, path)), want) == 0); \
} while (0)

int main(void)
{
	CHECK(strcmp(dirname(0), ".") == 0);
	T("", ".");
	T("/usr/lib", "/usr");
	T("/usr/", "/");
	T("usr", ".");
	T("usr/", ".");
	T("/", "/");
	T("///", "/");
	T(".", ".");
	T("..", ".");
	T("a//b//", "a");
	T("//a", "/");

	/* The result is the argument, cut short. */
	char buf[] = "one/two/three";
	CHECK(dirname(buf) == buf);
	CHECK(strcmp(buf, "one/two") == 0);
	return t_status;
}
