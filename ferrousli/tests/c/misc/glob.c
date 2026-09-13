/*
 * glob and globfree over a small tree: sorting, GLOB_MARK through a link,
 * GLOB_PERIOD, GLOB_NOCHECK, GLOB_DOOFFS, GLOB_APPEND, escapes, literal
 * paths, several wildcard components, the error callback with GLOB_ERR, and
 * GLOB_TILDE.
 *
 * The expectations follow musl, whose design this is. They were checked
 * against the host glibc 2.43 too, which agrees except where a comment says
 * it does not.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <glob.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "check.h"
#include "sys.h"

static glob_t g;

/* Whether the result is exactly the null-terminated list after the offset. */
static int is(size_t offs, const char *want[])
{
	size_t n = 0;
	while (want[n])
		n++;
	if (g.gl_pathc != n || g.gl_pathv[offs + n] != 0)
		return 0;
	for (size_t i = 0; i < n; i++)
		if (strcmp(g.gl_pathv[offs + i], want[i]) != 0)
			return 0;
	return 1;
}

#define LIST(...) ((const char *[]){__VA_ARGS__, 0})

static int calls, last_error;
static char last_path[64];

static int count_errors(const char *path, int error)
{
	calls++;
	last_error = error;
	strncpy(last_path, path, sizeof last_path - 1);
	return 0;
}

static int stop_on_error(const char *path, int error)
{
	return 1;
}

int main(void)
{
	CHECK(t_mkfile("a.c") == 0);
	CHECK(t_mkfile("b.c") == 0);
	CHECK(t_mkfile("c.h") == 0);
	CHECK(t_mkfile(".hidden.c") == 0);
	CHECK(t_mkfile("we[ird") == 0);
	CHECK(t_mkdir("dir") == 0);
	CHECK(t_mkfile("dir/x.c") == 0);
	CHECK(t_mkfile("dir/y.txt") == 0);
	CHECK(t_mkdir("dir/sub") == 0);
	CHECK(t_mkfile("dir/sub/z.c") == 0);
	CHECK(t_mkdir("dir2") == 0);
	CHECK(t_mkfile("dir2/x.c") == 0);
	CHECK(t_symlink("dir", "link") == 0);
	CHECK(t_symlink("nowhere", "dangling") == 0);

	/* Sorted, and a leading dot is not matched by a wildcard. */
	CHECK(glob("*.c", 0, 0, &g) == 0);
	CHECK(is(0, LIST("a.c", "b.c")));
	globfree(&g);
	/* musl empties the structure; glibc leaves the stale values. */
	CHECK(g.gl_pathc == 0 && g.gl_pathv == 0);

	/* GLOB_PERIOD lets it. */
	CHECK(glob("*.c", GLOB_PERIOD, 0, &g) == 0);
	CHECK(is(0, LIST(".hidden.c", "a.c", "b.c")));
	globfree(&g);

	/* A wildcard directory component, through a link too. */
	CHECK(glob("*/x.c", 0, 0, &g) == 0);
	CHECK(is(0, LIST("dir/x.c", "dir2/x.c", "link/x.c")));
	globfree(&g);
	CHECK(glob("dir/*/*.c", 0, 0, &g) == 0);
	CHECK(is(0, LIST("dir/sub/z.c")));
	globfree(&g);
	CHECK(glob("d?r*/[xy].*", 0, 0, &g) == 0);
	CHECK(is(0, LIST("dir/x.c", "dir/y.txt", "dir2/x.c")));
	globfree(&g);

	/* GLOB_MARK marks directories, a link to one included, and a dangling
	 * link is still found. */
	CHECK(glob("[dl]*", GLOB_MARK, 0, &g) == 0);
	CHECK(is(0, LIST("dangling", "dir/", "dir2/", "link/")));
	globfree(&g);
	CHECK(glob("dir", GLOB_MARK, 0, &g) == 0);
	CHECK(is(0, LIST("dir/")));
	globfree(&g);

	/* Literal paths exist or do not. */
	CHECK(glob("dir/sub/z.c", 0, 0, &g) == 0);
	CHECK(is(0, LIST("dir/sub/z.c")));
	globfree(&g);
	CHECK(glob("dir/", 0, 0, &g) == 0);
	CHECK(is(0, LIST("dir/")));
	globfree(&g);
	CHECK(glob("dir/sub/missing", 0, 0, &g) == GLOB_NOMATCH);
	CHECK(g.gl_pathc == 0);
	CHECK(glob("", 0, 0, &g) == GLOB_NOMATCH);

	/* GLOB_NOCHECK returns the pattern as it was written. */
	CHECK(glob("no\\*match*", GLOB_NOCHECK, 0, &g) == 0);
	CHECK(is(0, LIST("no\\*match*")));
	globfree(&g);

	/* Escapes. */
	CHECK(glob("we\\[ird", 0, 0, &g) == 0);
	CHECK(is(0, LIST("we[ird")));
	globfree(&g);
	CHECK(glob("we[[]ird*", 0, 0, &g) == 0);
	CHECK(is(0, LIST("we[ird")));
	globfree(&g);
	CHECK(glob("\\a.*", 0, 0, &g) == 0);
	CHECK(is(0, LIST("a.c")));
	globfree(&g);
	CHECK(glob("we\\[ird*", GLOB_NOESCAPE, 0, &g) == GLOB_NOMATCH);

	/* GLOB_DOOFFS reserves null slots, and GLOB_APPEND adds after them. */
	g.gl_offs = 2;
	CHECK(glob("?.c", GLOB_DOOFFS, 0, &g) == 0);
	CHECK(g.gl_pathv[0] == 0 && g.gl_pathv[1] == 0);
	CHECK(is(2, LIST("a.c", "b.c")));
	CHECK(glob("*.h", GLOB_DOOFFS | GLOB_APPEND, 0, &g) == 0);
	CHECK(is(2, LIST("a.c", "b.c", "c.h")));
	globfree(&g);

	/* GLOB_NOSORT finds the same paths: the eight entries without a leading
	 * dot, a dangling link among them. */
	CHECK(glob("*", GLOB_NOSORT, 0, &g) == 0);
	CHECK(g.gl_pathc == 8);
	globfree(&g);

	/* A directory that cannot be opened goes to the callback, and aborts
	 * with GLOB_ERR or when the callback asks. musl reports ENOENT and
	 * ENOTDIR as well; glibc calls the callback for neither, so the three
	 * checks of a.c and the callback's arguments differ from glibc. */
	calls = 0;
	CHECK(glob("missing/*", 0, count_errors, &g) == GLOB_NOMATCH);
	CHECK(calls == 1 && last_error == ENOENT && strcmp(last_path, "missing") == 0);
	CHECK(glob("missing/*", GLOB_ERR, 0, &g) == GLOB_ABORTED);
	CHECK(glob("a.c/*", 0, stop_on_error, &g) == GLOB_ABORTED);
	calls = 0;
	CHECK(glob("a.c/*", 0, count_errors, &g) == GLOB_NOMATCH);
	CHECK(calls == 1 && last_error == ENOTDIR);

	/* GLOB_TILDE expands $HOME. */
	setenv("HOME", "dir", 1);
	CHECK(glob("~/*.c", GLOB_TILDE, 0, &g) == 0);
	CHECK(is(0, LIST("dir/x.c")));
	globfree(&g);
	CHECK(glob("~", GLOB_TILDE | GLOB_MARK, 0, &g) == 0);
	CHECK(is(0, LIST("dir/")));
	globfree(&g);
	CHECK(glob("~no-such-user-here/x", GLOB_TILDE_CHECK, 0, &g) == GLOB_NOMATCH);
	/* Without GLOB_TILDE, ~ is an ordinary character. */
	CHECK(glob("~/*.c", GLOB_NOCHECK, 0, &g) == 0);
	CHECK(is(0, LIST("~/*.c")));
	globfree(&g);
	return t_status;
}
