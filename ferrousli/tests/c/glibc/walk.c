/*
 * Walking a tree the ways GLib, libselinux and libmount do: nftw and
 * nftw64 with glibc's type flags and FTW_ACTIONRETVAL, fts64 with its
 * order, fts_set's FTS_SKIP and FTS_AGAIN, and scandirat. The expected
 * sequences are glibc 2.43's for the same tree.
 */

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <ftw.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "check.h"

#ifdef __GLIBC__
#include <fts.h>
/* glibc's fts64 names take its 64 types, the same bytes here. */
#define FTS FTS64
#define FTSENT FTSENT64
#else
typedef struct _ftsent {
	struct _ftsent *fts_cycle, *fts_parent, *fts_link;
	long fts_number;
	void *fts_pointer;
	char *fts_accpath, *fts_path;
	int fts_errno, fts_symfd;
	unsigned short fts_pathlen, fts_namelen;
	unsigned long long fts_ino, fts_dev;
#if defined(__x86_64__)
	unsigned long fts_nlink;
#else
	unsigned int fts_nlink;
#endif
	short fts_level;
	unsigned short fts_info, fts_flags, fts_instr;
	struct stat *fts_statp;
	char fts_name[1];
} FTSENT;
typedef struct { FTSENT *fts_cur; } FTS;
#define FTS_PHYSICAL 0x10
#define FTS_NOCHDIR 0x04
#define FTS_D 1
#define FTS_DP 6
#define FTS_F 8
#define FTS_SL 12
#define FTS_SKIP 4
#define FTS_AGAIN 1
FTS *fts64_open(char *const *, int, int (*)(const FTSENT **, const FTSENT **));
FTSENT *fts64_read(FTS *);
int fts64_set(FTS *, FTSENT *, int);
int fts64_close(FTS *);
int scandirat(int, const char *, struct dirent ***, int (*)(const struct dirent *),
	int (*)(const struct dirent **, const struct dirent **));
int nftw64(const char *, int (*)(const char *, const struct stat *, int, struct FTW *), int, int);
#endif

static char seen[512];

static void note(const char *what)
{
	strcat(seen, what);
	strcat(seen, " ");
}

static int visit(const char *path, const struct stat *st, int type, struct FTW *ftw)
{
	char item[64];
	(void)st;
	snprintf(item, sizeof item, "%s:%d:%d", path + ftw->base, type, ftw->level);
	note(item);
	return 0;
}

static int skip_b(const char *path, const struct stat *st, int type, struct FTW *ftw)
{
	(void)st;
	(void)type;
	note(path + ftw->base);
	return strcmp(path + ftw->base, "b") == 0 ? FTW_SKIP_SUBTREE : FTW_CONTINUE;
}

static int stop_at_a(const char *path, const struct stat *st, int type, struct FTW *ftw)
{
	(void)st;
	(void)type;
	return strcmp(path + ftw->base, "a") == 0 ? 42 : 0;
}

static int by_name(const FTSENT **a, const FTSENT **b)
{
	return strcmp((*a)->fts_name, (*b)->fts_name);
}

static int no_dots(const struct dirent *d)
{
	return d->d_name[0] != '.';
}

static int alpha(const struct dirent **a, const struct dirent **b)
{
	return strcmp((*a)->d_name, (*b)->d_name);
}

int main(void)
{
	CHECK(mkdir("d", 0755) == 0 && mkdir("d/b", 0755) == 0);
	CHECK(close(open("d/a", O_CREAT | O_WRONLY, 0644)) == 0);
	CHECK(close(open("d/b/c", O_CREAT | O_WRONLY, 0644)) == 0);
	CHECK(symlink("a", "d/l") == 0);

	/* nftw's order follows the directory; compare sorted pieces instead. */
	seen[0] = 0;
	CHECK(nftw("d", visit, 8, FTW_PHYS) == 0);
	CHECK(strstr(seen, "d:1:0 ") == seen);
	CHECK(strstr(seen, "a:0:1 ") && strstr(seen, "b:1:1 ") && strstr(seen, "c:0:2 "));
	CHECK(strstr(seen, "l:4:1 "));

	seen[0] = 0;
#ifdef __GLIBC__
	/* glibc's nftw64 takes a struct stat64, the same bytes here. */
	CHECK(nftw64("d", (__nftw64_func_t)visit, 8, FTW_PHYS | FTW_DEPTH) == 0);
#else
	CHECK(nftw64("d", visit, 8, FTW_PHYS | FTW_DEPTH) == 0);
#endif
	CHECK(strstr(seen, "b:5:1 ") && strstr(seen, "d:5:0 "));
	CHECK(strstr(seen, "c:0:2 ") < strstr(seen, "b:5:1 "));
	CHECK(strstr(seen, "d:5:0 ") == seen + strlen(seen) - strlen("d:5:0 "));

	/* Followed, the link is the file it names. */
	seen[0] = 0;
	CHECK(nftw("d", visit, 8, 0) == 0);
	CHECK(strstr(seen, "l:0:1 "));

	seen[0] = 0;
	CHECK(nftw("d", skip_b, 8, FTW_PHYS | FTW_ACTIONRETVAL) == 0);
	CHECK(strstr(seen, "b ") && !strstr(seen, "c "));
	CHECK(nftw("d", stop_at_a, 8, FTW_PHYS) == 42);

	/* fts, in name order: not on ARMv7-A, where glibc's structure differs. */
#if !defined(__arm__)
	{
		char *paths[] = { "d", NULL };
		FTS *fts = fts64_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, by_name);
		FTSENT *e;
		CHECK(fts != NULL);
		seen[0] = 0;
		while ((e = fts64_read(fts))) {
			char item[64];
			snprintf(item, sizeof item, "%s:%d:%d", e->fts_name, e->fts_info, e->fts_level);
			note(item);
			if (e->fts_info == FTS_F && strcmp(e->fts_name, "a") == 0) {
				CHECK(strcmp(e->fts_path, "d/a") == 0);
				CHECK(S_ISREG(e->fts_statp->st_mode));
			}
		}
		CHECK(errno == 0);
		CHECK(strcmp(seen, "d:1:0 a:8:1 b:1:1 c:8:2 b:6:1 l:12:1 d:6:0 ") == 0);
		CHECK(fts64_close(fts) == 0);

		fts = fts64_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, by_name);
		CHECK(fts != NULL);
		seen[0] = 0;
		int again = 1;
		while ((e = fts64_read(fts))) {
			note(e->fts_name);
			if (strcmp(e->fts_name, "b") == 0 && e->fts_info == FTS_D)
				CHECK(fts64_set(fts, e, FTS_SKIP) == 0);
			if (strcmp(e->fts_name, "a") == 0 && again) {
				again = 0;
				CHECK(fts64_set(fts, e, FTS_AGAIN) == 0);
			}
		}
		CHECK(strcmp(seen, "d a a b b l d ") == 0);
		CHECK(fts64_close(fts) == 0);
	}
#endif

	/* scandirat, relative to a directory descriptor. */
	{
		struct dirent **names;
		int dfd = open("d", O_RDONLY | O_DIRECTORY);
		CHECK(dfd >= 0);
		int n = scandirat(dfd, "b", &names, no_dots, alpha);
		CHECK(n == 1 && strcmp(names[0]->d_name, "c") == 0);
		while (n-- > 0)
			free(names[n]);
		free(names);
		n = scandirat(dfd, ".", &names, no_dots, alpha);
		CHECK(n == 3 && strcmp(names[0]->d_name, "a") == 0 && strcmp(names[2]->d_name, "l") == 0);
		while (n-- > 0)
			free(names[n]);
		free(names);
		errno = 0;
		CHECK(scandirat(dfd, "missing", &names, NULL, NULL) == -1 && errno == ENOENT);
		close(dfd);
	}
	return t_status;
}
