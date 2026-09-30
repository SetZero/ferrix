/*
 * realpath: the root, the working directory, relative and absolute
 * symbolic links, . and .. and repeated slashes, a caller's buffer and a
 * returned one, and the errors: a file named as a directory, a missing
 * component, a link loop, an empty name and .. above the working directory.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	char cwd[PATH_MAX], want[PATH_MAX], got[PATH_MAX], *p;
	int fd;

	CHECK(getcwd(cwd, sizeof cwd) != 0);
	CHECK(mkdir("dir", 0700) == 0 && mkdir("dir/sub", 0700) == 0);
	fd = open("dir/file", O_WRONLY | O_CREAT, 0600);
	CHECK(fd >= 0);
	close(fd);
	CHECK(symlink("sub", "dir/rel") == 0);
	CHECK(symlink(cwd, "abs") == 0);
	CHECK(symlink("loop2", "loop1") == 0 && symlink("loop1", "loop2") == 0);

	CHECK(realpath("/", got) == got && !strcmp(got, "/"));
	CHECK(realpath(".", got) == got && !strcmp(got, cwd));

	strcpy(want, cwd);
	strcat(want, "/dir/sub");
	CHECK(realpath("dir/rel", got) && !strcmp(got, want));
	CHECK(realpath("./dir//sub/.", got) && !strcmp(got, want));
	CHECK(realpath("abs/dir/rel/../rel", got) && !strcmp(got, want));
	CHECK(realpath("dir/rel/", got) && !strcmp(got, want));

	strcpy(want, cwd);
	strcat(want, "/dir/file");
	p = realpath("dir/../dir/file", 0);
	CHECK(p && !strcmp(p, want));
	free(p);

	errno = 0;
	CHECK(!realpath("dir/file/", got) && errno == ENOTDIR);
	errno = 0;
	CHECK(!realpath("missing/x", got) && errno == ENOENT);
	errno = 0;
	CHECK(!realpath("loop1", got) && errno == ELOOP);
	errno = 0;
	CHECK(!realpath("", got) && errno == ENOENT);
	errno = 0;
	CHECK(!realpath(0, got) && errno == EINVAL);

	strcpy(want, cwd);
	p = strrchr(want, '/');
	if (p)
		*(p == want ? p + 1 : p) = 0;
	CHECK(realpath("..", got) && !strcmp(got, want));
	return t_status;
}
