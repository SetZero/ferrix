/*
 * basename, adapted from libc-test's src/functional/basename.c (MIT).
 */

#include <libgen.h>
#include <string.h>
#include "check.h"

#define T(path, want) do { \
	char tmp[100]; \
	CHECK(strcmp(basename(strcpy(tmp, path)), want) == 0); \
} while (0)

int main(void)
{
	CHECK(strcmp(basename(0), ".") == 0);
	T("", ".");
	T("/usr/lib", "lib");
	T("/usr/", "usr");
	T("usr/", "usr");
	T("/", "/");
	T("///", "/");
	T("//usr//lib//", "lib");
	T(".", ".");
	T("..", "..");
	T("a", "a");

	/* The trailing slashes are overwritten in place. */
	char buf[] = "dir/file//";
	char *base = basename(buf);
	CHECK(base == buf + 4);
	CHECK(strcmp(buf, "dir/file") == 0);
	return t_status;
}
