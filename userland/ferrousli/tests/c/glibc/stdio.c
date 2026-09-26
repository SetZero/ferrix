/*
 * A stream as a program built against glibc's headers reaches it: the
 * inline getc_unlocked, putc_unlocked, feof_unlocked and ferror_unlocked
 * that glibc 2.43's <bits/types/struct_FILE.h> and <bits/stdio.h> expand,
 * written out here over the start of glibc's struct _IO_FILE; and the
 * stdio_ext.h functions and glibc's internal names beside them.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdio_ext.h>
#include <stdlib.h>
#include <string.h>

#include "check.h"

/* The fields glibc's macros read, at the offsets its headers compile in. */
struct glibc_file {
	int flags;
	char *read_ptr, *read_end, *read_base;
	char *write_base, *write_ptr, *write_end;
};

int __uflow(FILE *);
int __overflow(FILE *, int);
ssize_t __getdelim(char **, size_t *, int, FILE *);
FILE *tmpfile64(void);

static int glibc_getc(FILE *f)
{
	struct glibc_file *g = (struct glibc_file *)f;
	return g->read_ptr >= g->read_end ? __uflow(f) : *(unsigned char *)g->read_ptr++;
}

static int glibc_putc(int c, FILE *f)
{
	struct glibc_file *g = (struct glibc_file *)f;
	return g->write_ptr >= g->write_end ? __overflow(f, (unsigned char)c)
		: (unsigned char)(*g->write_ptr++ = (char)c);
}

static int glibc_feof(FILE *f)
{
	return (((struct glibc_file *)f)->flags & 0x10) != 0;
}

static int glibc_ferror(FILE *f)
{
	return (((struct glibc_file *)f)->flags & 0x20) != 0;
}

int main(void)
{
	char *line = NULL;
	size_t cap = 0;
	FILE *f = tmpfile64();

	CHECK(f != NULL);
	CHECK(__fsetlocking(f, FSETLOCKING_BYCALLER) == FSETLOCKING_INTERNAL);
	CHECK(__freadable(f) && __fwritable(f));

	for (const char *p = "one\ntwo:three"; *p; p++)
		CHECK(glibc_putc(*p, f) == (unsigned char)*p);
	CHECK(__fwriting(f) && !__freading(f));
	CHECK(__fpending(f) == 13);
	CHECK(__fbufsize(f) > 0);
	CHECK(__overflow(f, EOF) == 0);
	CHECK(__fpending(f) == 0);

	rewind(f);
	CHECK(glibc_getc(f) == 'o' && glibc_getc(f) == 'n' && glibc_getc(f) == 'e');
	CHECK(__freading(f) && !__fwriting(f));
#ifndef __GLIBC__
	/* musl's, which glibc lacks. */
	CHECK(__freadahead(f) == 10);
#endif
	CHECK(glibc_getc(f) == '\n');
	CHECK(__getdelim(&line, &cap, ':', f) == 4 && strcmp(line, "two:") == 0);
	CHECK(!glibc_feof(f));
	CHECK(__getdelim(&line, &cap, ':', f) == 5 && strcmp(line, "three") == 0);
	CHECK(glibc_getc(f) == EOF);
	CHECK(glibc_feof(f) && feof(f) && !glibc_ferror(f));
	clearerr(f);
	CHECK(!glibc_feof(f));
#ifndef __GLIBC__
	__fseterr(f);
	CHECK(glibc_ferror(f) && ferror(f));
	clearerr(f);
#endif

	rewind(f);
	CHECK(glibc_getc(f) == 'o');
	__fpurge(f);
#ifndef __GLIBC__
	CHECK(__freadahead(f) == 0);
#endif
	fclose(f);

	CHECK(!__flbf(stderr));
	free(line);
	return t_status;
}
