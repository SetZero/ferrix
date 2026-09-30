/*
 * Standard input from a pipe, standard error, and perror. The harness feeds
 * "first line\nsecond\nthird\nrest of it".
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "test.h"

#define CHECK(c) do { \
	if (!(c)) \
		t_error("%s failed (errno %d)\n", #c, errno); \
} while (0)

int main(void)
{
	char buf[64];
	char *line = NULL;
	size_t cap = 0;

	CHECK(getchar() == 'f');
	CHECK(ungetc('F', stdin) == 'F');
	CHECK(fgets(buf, sizeof buf, stdin) == buf && strcmp(buf, "First line\n") == 0);
	CHECK(getline(&line, &cap, stdin) == 7 && strcmp(line, "second\n") == 0);
	free(line);
	CHECK(fread(buf, 1, 6, stdin) == 6 && memcmp(buf, "third\n", 6) == 0);
	errno = 0;
	CHECK(ftell(stdin) == -1 && errno == ESPIPE);
	CHECK(fread(buf, 1, sizeof buf, stdin) == 10 && memcmp(buf, "rest of it", 10) == 0);
	CHECK(feof(stdin) && !ferror(stdin));
	CHECK(getchar() == EOF);
	clearerr(stdin);
	CHECK(!feof(stdin));
	CHECK(getchar() == EOF && feof(stdin));

	errno = EACCES;
	perror("stdin");
	CHECK(errno == EACCES);
	errno = 0;
	perror(NULL);
	errno = ENOENT;
	perror("");
	CHECK(fputs("to stderr\n", stderr) >= 0);
	CHECK(puts("done") >= 0);
	return t_status;
}
