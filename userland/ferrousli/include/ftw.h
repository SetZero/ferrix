#ifndef _FTW_H
#define	_FTW_H

#ifdef __cplusplus
extern "C" {
#endif

#include <features.h>
#include <sys/stat.h>

/* Ferrousli, not musl 1.2.5: the type flags are glibc's, from 0, where
 * musl's run from 1, because a program built against glibc calls the same
 * nftw and compares with glibc's numbers. POSIX leaves the values open. */
#define FTW_F   0
#define FTW_D   1
#define FTW_DNR 2
#define FTW_NS  3
#define FTW_SL  4
#define FTW_DP  5
#define FTW_SLN 6

#define FTW_PHYS  1
#define FTW_MOUNT 2
#define FTW_CHDIR 4
#define FTW_DEPTH 8

#ifdef _GNU_SOURCE
/* Ferrousli, not musl 1.2.5: glibc's FTW_ACTIONRETVAL and its answers. */
#define FTW_ACTIONRETVAL 16
#define FTW_CONTINUE 0
#define FTW_STOP 1
#define FTW_SKIP_SUBTREE 2
#define FTW_SKIP_SIBLINGS 3
#endif

struct FTW {
	int base;
	int level;
};

int ftw(const char *, int (*)(const char *, const struct stat *, int), int);
int nftw(const char *, int (*)(const char *, const struct stat *, int, struct FTW *), int, int);

#if defined(_LARGEFILE64_SOURCE)
#define ftw64 ftw
#define nftw64 nftw
#endif

#ifdef __cplusplus
}
#endif

#endif
