/* kconfig uses regex only for menuconfig's symbol search; always "no match". */
#ifndef HOSTCOMPAT_REGEX_H
#define HOSTCOMPAT_REGEX_H
#include <stddef.h>

typedef struct { int unused; } regex_t;
typedef struct { int rm_so, rm_eo; } regmatch_t;
#define REG_EXTENDED 1
#define REG_NOSUB 2
#define REG_ICASE 4

static inline int regcomp(regex_t *re, const char *pattern, int flags)
{
	(void)re; (void)pattern; (void)flags;
	return 1;
}

static inline int regexec(const regex_t *re, const char *s, size_t n, regmatch_t *m, int flags)
{
	(void)re; (void)s; (void)n; (void)m; (void)flags;
	return 1;
}

static inline void regfree(regex_t *re)
{
	(void)re;
}
#endif
