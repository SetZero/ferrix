/*
 * Temporary files and directories: names made from the template, created
 * exclusively with their owner's permissions, a suffix, extra flags, the
 * template left alone when it is refused, and mktemp's unchecked name.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "check.h"

static int letters_only(const char *s, size_t n)
{
	for (size_t i = 0; i < n; i++)
		if (!((s[i] >= 'A' && s[i] <= 'P') || (s[i] >= 'a' && s[i] <= 'p')))
			return 0;
	return 1;
}

int main(void)
{
	char a[] = "fileXXXXXX", b[] = "fileXXXXXX", s[] = "partXXXXXX.txt";
	char c[] = "cloXXXXXX", d[] = "dirXXXXXX", m[] = "nameXXXXXX";
	char bad[] = "fileXXXXX", long_suffix[] = "XXXXXXab", few[] = "XXXXX";
	struct stat st;
	int fa, fb, fs, fc;

	umask(022);

	fa = mkstemp(a);
	CHECK(fa >= 0 && !strncmp(a, "file", 4) && letters_only(a + 4, 6));
	CHECK(fstat(fa, &st) == 0 && S_ISREG(st.st_mode) && (st.st_mode & 0777) == 0600);
	CHECK(write(fa, "x", 1) == 1 && lseek(fa, 0, SEEK_SET) == 0);
	CHECK(!(fcntl(fa, F_GETFD) & FD_CLOEXEC));
	fb = mkstemp(b);
	CHECK(fb >= 0 && strcmp(a, b) != 0);

	fs = mkstemps(s, 4);
	CHECK(fs >= 0 && !strncmp(s, "part", 4) && letters_only(s + 4, 6) && !strcmp(s + 10, ".txt"));
	CHECK(stat(s, &st) == 0);

	fc = mkostemp(c, O_CLOEXEC);
	CHECK(fc >= 0 && (fcntl(fc, F_GETFD) & FD_CLOEXEC));

	CHECK(mkdtemp(d) == d && letters_only(d + 3, 6));
	CHECK(stat(d, &st) == 0 && S_ISDIR(st.st_mode) && (st.st_mode & 0777) == 0700);

	errno = 0;
	CHECK(mkstemp(bad) == -1 && errno == EINVAL && !strcmp(bad, "fileXXXXX"));
	errno = 0;
	CHECK(mkstemps(long_suffix, 3) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(!mkdtemp(few) && errno == EINVAL && !strcmp(few, "XXXXX"));

	CHECK(mktemp(m) == m && !strncmp(m, "name", 4) && letters_only(m + 4, 6));
	errno = 0;
	CHECK(stat(m, &st) == -1 && errno == ENOENT);
	errno = 0;
	CHECK(mktemp(few) == few && few[0] == 0 && errno == EINVAL);

	close(fa);
	close(fb);
	close(fs);
	close(fc);
	return t_status;
}
