#ifndef _REGEX_H
#define _REGEX_H

#ifdef __cplusplus
extern "C" {
#endif

#include <features.h>

#define __NEED_size_t

#include <bits/alltypes.h>

/* glibc's layouts, not musl's: see src/regex.rs. */
typedef int regoff_t;
typedef unsigned long reg_syntax_t;

/* GNU's names for the fields, under _GNU_SOURCE; reserved ones otherwise. */
#ifdef _GNU_SOURCE
#define __REPB(name) name
#else
#define __REPB(name) __##name
#endif

typedef struct re_pattern_buffer {
	void *__REPB(buffer);
	unsigned long __REPB(allocated);
	unsigned long __REPB(used);
	reg_syntax_t __REPB(syntax);
	char *__REPB(fastmap);
	unsigned char *__REPB(translate);
	size_t re_nsub;
	unsigned __REPB(can_be_null) : 1;
	unsigned __REPB(regs_allocated) : 2;
	unsigned __REPB(fastmap_accurate) : 1;
	unsigned __REPB(no_sub) : 1;
	unsigned __REPB(not_bol) : 1;
	unsigned __REPB(not_eol) : 1;
	unsigned __REPB(newline_anchor) : 1;
} regex_t;

#undef __REPB

typedef struct {
	regoff_t rm_so;
	regoff_t rm_eo;
} regmatch_t;

#define REG_EXTENDED    1
#define REG_ICASE       2
#define REG_NEWLINE     4
#define REG_NOSUB       8

#define REG_NOTBOL      1
#define REG_NOTEOL      2
#define REG_STARTEND    4

#define REG_OK          0
#define REG_NOMATCH     1
#define REG_BADPAT      2
#define REG_ECOLLATE    3
#define REG_ECTYPE      4
#define REG_EESCAPE     5
#define REG_ESUBREG     6
#define REG_EBRACK      7
#define REG_EPAREN      8
#define REG_EBRACE      9
#define REG_BADBR       10
#define REG_ERANGE      11
#define REG_ESPACE      12
#define REG_BADRPT      13

#define REG_ENOSYS      -1

int regcomp(regex_t *__restrict, const char *__restrict, int);
int regexec(const regex_t *__restrict, const char *__restrict, size_t, regmatch_t *__restrict, int);
void regfree(regex_t *);

size_t regerror(int, const regex_t *__restrict, char *__restrict, size_t);

#ifdef _GNU_SOURCE
/* GNU's own interface, over the same engine. Of the syntax bits, the
   language (RE_NO_BK_PARENS), RE_ICASE and RE_NO_SUB are read. */
#define RE_NO_BK_PARENS (1UL << 13)
#define RE_ICASE        (1UL << 22)
#define RE_NO_SUB       (1UL << 25)
#define REGS_UNALLOCATED 0
#define REGS_REALLOCATE  1
#define REGS_FIXED       2

struct re_registers {
	unsigned num_regs;
	regoff_t *start;
	regoff_t *end;
};

extern reg_syntax_t re_syntax_options;
reg_syntax_t re_set_syntax(reg_syntax_t);
const char *re_compile_pattern(const char *, size_t, struct re_pattern_buffer *);
regoff_t re_search(struct re_pattern_buffer *, const char *, regoff_t, regoff_t, regoff_t,
                   struct re_registers *);
#endif

#ifdef __cplusplus
}
#endif

#endif
