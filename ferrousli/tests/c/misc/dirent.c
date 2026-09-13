/*
 * dirent.h: reading a directory with more entries than one getdents64
 * buffer holds, positions, readdir_r, scandir with its sorts, and the errors.
 */

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"
#include "sys.h"

#define FILES 1000

/* "a-file-name-long-enough-to-fill-buffers-NNNN" */
static void name_of(int i, char *buf)
{
	static const char prefix[] = "a-file-name-long-enough-to-fill-buffers-";
	memcpy(buf, prefix, sizeof prefix - 1);
	buf += sizeof prefix - 1;
	buf[0] = '0' + i / 1000;
	buf[1] = '0' + i / 100 % 10;
	buf[2] = '0' + i / 10 % 10;
	buf[3] = '0' + i % 10;
	buf[4] = 0;
}

/* The file number in a name, or -1 for another name. */
static int number_of(const char *name)
{
	char want[64];
	int i;
	if (strlen(name) != 44)
		return -1;
	i = (name[40] - '0') * 1000 + (name[41] - '0') * 100 +
		(name[42] - '0') * 10 + (name[43] - '0');
	if (i < 0 || i >= FILES)
		return -1;
	name_of(i, want);
	return strcmp(want, name) == 0 ? i : -1;
}

static char seen[FILES];

/* Reads the stream to its end, checking each name appears once. */
static int read_all(DIR *d)
{
	struct dirent *de;
	int n = 0, dots = 0;
	memset(seen, 0, sizeof seen);
	errno = 0;
	while ((de = readdir(d))) {
		int i = number_of(de->d_name);
		n++;
		if (!strcmp(de->d_name, ".") || !strcmp(de->d_name, "..")) {
			dots++;
			CHECK(de->d_type == DT_DIR || de->d_type == DT_UNKNOWN);
			continue;
		}
		CHECK(i >= 0);
		if (i >= 0) {
			CHECK(!seen[i]);
			seen[i] = 1;
		}
		CHECK(de->d_type == DT_REG || de->d_type == DT_UNKNOWN);
		CHECK(de->d_reclen >= 19 + 45);
	}
	CHECK(errno == 0);
	CHECK(dots == 2);
	return n;
}

static int no_dots(const struct dirent *de)
{
	return de->d_name[0] != '.';
}

int main(void)
{
	char name[64];
	DIR *d;
	struct dirent *de;

	/* An empty directory holds only . and .. */
	d = opendir(".");
	CHECK(d != 0);
	CHECK(dirfd(d) >= 0);
	int n = 0;
	errno = 0;
	while (readdir(d))
		n++;
	CHECK(n == 2);
	CHECK(errno == 0);
	CHECK(readdir(d) == 0);
	CHECK(closedir(d) == 0);

	for (int i = 0; i < FILES; i++) {
		name_of(i, name);
		CHECK(t_mkfile(name) == 0);
	}

	/* 1000 records of 64 bytes take several buffers. */
	d = opendir(".");
	CHECK(d != 0);
	CHECK(read_all(d) == FILES + 2);
	for (int i = 0; i < FILES; i++)
		CHECK(seen[i]);

	/* rewinddir starts again. */
	rewinddir(d);
	CHECK(read_all(d) == FILES + 2);

	/* telldir and seekdir return to an entry, across a buffer refill. */
	rewinddir(d);
	for (int i = 0; i < 500; i++)
		CHECK(readdir(d) != 0);
	long pos = telldir(d);
	de = readdir(d);
	CHECK(de != 0);
	char expected[256];
	strcpy(expected, de ? de->d_name : "");
	for (int i = 0; i < 300; i++)
		CHECK(readdir(d) != 0);
	seekdir(d, pos);
	CHECK(telldir(d) == pos);
	de = readdir(d);
	CHECK(de && strcmp(de->d_name, expected) == 0);

	/* readdir_r copies the entry, and reports the end with a null result. */
	rewinddir(d);
	struct dirent buf, *result;
	int count = 0;
	for (;;) {
		CHECK(readdir_r(d, &buf, &result) == 0);
		if (!result)
			break;
		CHECK(result == &buf);
		count++;
	}
	CHECK(count == FILES + 2);
	CHECK(closedir(d) == 0);

	/* scandir filters and sorts, and alphasort orders by name. */
	struct dirent **list;
	n = scandir(".", &list, no_dots, alphasort);
	CHECK(n == FILES);
	for (int i = 0; i < n; i++) {
		CHECK(number_of(list[i]->d_name) == i);
		free(list[i]);
	}
	free(list);

	/* Without a filter or a sort, everything comes back. */
	n = scandir(".", &list, 0, 0);
	CHECK(n == FILES + 2);
	for (int i = 0; i < n; i++)
		free(list[i]);
	free(list);

	/* versionsort orders numbers by value. */
	CHECK(t_mkdir("v") == 0);
	CHECK(t_mkfile("v/x10") == 0);
	CHECK(t_mkfile("v/x9") == 0);
	CHECK(t_mkfile("v/x1") == 0);
	CHECK(t_mkfile("v/x09") == 0);
	n = scandir("v", &list, no_dots, versionsort);
	CHECK(n == 4);
	if (n == 4) {
		CHECK(!strcmp(list[0]->d_name, "x09"));
		CHECK(!strcmp(list[1]->d_name, "x1"));
		CHECK(!strcmp(list[2]->d_name, "x9"));
		CHECK(!strcmp(list[3]->d_name, "x10"));
	}
	for (int i = 0; i < n; i++)
		free(list[i]);
	free(list);

	/* An empty result is still an array. */
	CHECK(t_mkdir("empty") == 0);
	n = scandir("empty", &list, no_dots, alphasort);
	CHECK(n == 0);
	free(list);

	/* Errors. */
	errno = 0;
	CHECK(opendir("missing") == 0 && errno == ENOENT);
	errno = 0;
	CHECK(opendir("v/x1") == 0 && errno == ENOTDIR);
	errno = 0;
	CHECK(scandir("missing", &list, 0, 0) == -1 && errno == ENOENT);

	int fd = t_open("v/x1", O_RDONLY);
	CHECK(fd >= 0);
	errno = 0;
	CHECK(fdopendir(fd) == 0 && errno == ENOTDIR);
	t_close(fd);
	errno = 0;
	CHECK(fdopendir(fd) == 0 && errno == EBADF);
	fd = t_open("v", O_RDONLY | O_PATH);
	CHECK(fd >= 0);
	errno = 0;
	CHECK(fdopendir(fd) == 0 && errno == EBADF);
	t_close(fd);

	/* fdopendir takes over a directory's descriptor. */
	fd = t_open("v", O_RDONLY | O_DIRECTORY);
	CHECK(fd >= 0);
	d = fdopendir(fd);
	CHECK(d != 0);
	if (d) {
		CHECK(dirfd(d) == fd);
		n = 0;
		while ((de = readdir(d)))
			n++;
		CHECK(n == 6);
		CHECK(closedir(d) == 0);
	}
	return t_status;
}
