/*
 * catopen, catgets and catclose over a catalogue in gencat's format, written
 * here byte by byte: one set, 1, with messages 1 and 7.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <nl_types.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "check.h"

static void put(unsigned char *at, unsigned value)
{
	at[0] = value >> 24;
	at[1] = value >> 16;
	at[2] = value >> 8;
	at[3] = value;
}

int main(void)
{
	unsigned char file[68] = {0};
	put(file, 0xff88ff89);
	put(file + 4, 1);           /* sets */
	put(file + 8, 48);          /* size less the header */
	put(file + 12, 12);         /* messages, after the header */
	put(file + 16, 36);         /* strings, after the header */
	put(file + 20, 1);          /* set 1 */
	put(file + 24, 2);          /* two messages */
	put(file + 28, 0);          /* from the first */
	put(file + 32, 1);          /* message 1 */
	put(file + 36, 5);
	put(file + 40, 0);
	put(file + 44, 7);          /* message 7 */
	put(file + 48, 5);
	put(file + 52, 6);
	memcpy(file + 56, "hello\0world\0", 12);
	int fd = open("app.cat", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	CHECK(fd >= 0);
	CHECK(write(fd, file, sizeof file) == (ssize_t)sizeof file);
	CHECK(close(fd) == 0);

	/* No NLSPATH, and a bare name, finds nothing. */
	errno = 0;
	CHECK(catopen("app", 0) == (nl_catd)-1 && errno == ENOENT);

	/* A path is opened as it is. */
	nl_catd cat = catopen("./app.cat", 0);
	CHECK(cat != (nl_catd)-1);
	if (cat != (nl_catd)-1) {
		CHECK(strcmp(catgets(cat, 1, 1, "x"), "hello") == 0);
		CHECK(strcmp(catgets(cat, 1, 7, "x"), "world") == 0);
		errno = 0;
		CHECK(strcmp(catgets(cat, 1, 3, "fallback"), "fallback") == 0 && errno == ENOMSG);
		errno = 0;
		CHECK(strcmp(catgets(cat, 2, 1, "fallback"), "fallback") == 0 && errno == ENOMSG);
		CHECK(catclose(cat) == 0);
	}

	/* Along NLSPATH: an unknown substitution and a missing file are passed over. */
	CHECK(setenv("NLSPATH", "%q/%N:missing/%N:%N.%c.cat:%N.cat", 1) == 0);
	CHECK(setenv("LANG", "de_AT.UTF-8", 1) == 0);
	cat = catopen("app", 0);
	CHECK(cat != (nl_catd)-1);
	if (cat != (nl_catd)-1) {
		CHECK(strcmp(catgets(cat, 1, 1, "x"), "hello") == 0);
		CHECK(catclose(cat) == 0);
	}

	/* A file that is not a catalogue. */
	fd = open("bad.cat", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	CHECK(fd >= 0);
	CHECK(write(fd, file + 4, 64) == 64);
	CHECK(close(fd) == 0);
	errno = 0;
	CHECK(catopen("./bad.cat", 0) == (nl_catd)-1 && errno == ENOENT);
	return t_status;
}
