/*
 * mntent.h: fields, defaults, comments, escapes, a line too long for the
 * buffer, and tables in a file and in /proc.
 *
 * The first three cases are adapted from libc-test's functional/mntent.c
 * (MIT).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <mntent.h>
#include <stdio.h>
#include <string.h>

#include "check.h"

static FILE *table(const char *text)
{
	return fmemopen((void *)text, strlen(text), "r");
}

int main(void)
{
	struct mntent m, *r;
	char buf[64], small[16];
	FILE *f;
	int count;

	/* An empty table. */
	f = table("\n");
	CHECK(f && !getmntent(f));
	CHECK(endmntent(f) == 1);

	/* The fifth and sixth fields default to 0. */
	f = table("none /proc proc defaults\n");
	r = getmntent(f);
	CHECK(r);
	if (r) {
		CHECK(!strcmp(r->mnt_fsname, "none") && !strcmp(r->mnt_dir, "/proc"));
		CHECK(!strcmp(r->mnt_type, "proc") && !strcmp(r->mnt_opts, "defaults"));
		CHECK(r->mnt_freq == 0 && r->mnt_passno == 0);
	}
	CHECK(!getmntent(f));
	CHECK(endmntent(f) == 1);

	/* Tabs, and the caller's buffer. */
	f = table("/dev/sda\t/\text4\trw,nosuid\t2\t1\n");
	r = getmntent_r(f, &m, buf, sizeof buf);
	CHECK(r == &m);
	CHECK(!strcmp(m.mnt_fsname, "/dev/sda") && !strcmp(m.mnt_dir, "/"));
	CHECK(!strcmp(m.mnt_type, "ext4") && !strcmp(m.mnt_opts, "rw,nosuid"));
	CHECK(m.mnt_freq == 2 && m.mnt_passno == 1);
	CHECK(m.mnt_fsname >= buf && m.mnt_opts < buf + sizeof buf);
	CHECK(endmntent(f) == 1);

	/* Comments and blank lines are skipped, escapes decoded, and numbers
	 * belong to their own line. */
	f = table("# a comment 5 6\n"
		  "\n"
		  "   \n"
		  "/dev/disk\\040one /mnt/a\\040b\\\\c ext4 rw,noatime 0 2\n"
		  "  tmpfs\t/tmp tmpfs\tmode=1777\n"
		  "a\\0b /x\\9 t o 3 4\n");
	r = getmntent(f);
	CHECK(r);
	if (r) {
		CHECK(!strcmp(r->mnt_fsname, "/dev/disk one"));
		CHECK(!strcmp(r->mnt_dir, "/mnt/a b\\c"));
		CHECK(!strcmp(r->mnt_opts, "rw,noatime"));
		CHECK(r->mnt_freq == 0 && r->mnt_passno == 2);
		CHECK(hasmntopt(r, "noatime") == r->mnt_opts + 3);
		CHECK(hasmntopt(r, "sync") == 0);
	}
	r = getmntent(f);
	CHECK(r);
	if (r) {
		CHECK(!strcmp(r->mnt_fsname, "tmpfs") && !strcmp(r->mnt_dir, "/tmp"));
		CHECK(!strcmp(r->mnt_type, "tmpfs") && !strcmp(r->mnt_opts, "mode=1777"));
		CHECK(r->mnt_freq == 0 && r->mnt_passno == 0);
	}
	r = getmntent(f);
	CHECK(r);
	if (r) {
		CHECK(!strcmp(r->mnt_fsname, "a\\0b") && !strcmp(r->mnt_dir, "/x\\9"));
		CHECK(r->mnt_freq == 3 && r->mnt_passno == 4);
	}
	CHECK(!getmntent(f));
	CHECK(endmntent(f) == 1);

	/* A line too long for the buffer fails, and the next is read: it fills
	 * the sixteen bytes exactly, newline and NUL included. */
	f = table("/dev/a-long-device-name /mnt ext4 rw 0 0\ntmp /s t o 1 1\n");
	errno = 0;
	CHECK(!getmntent_r(f, &m, small, sizeof small) && errno == ERANGE);
	r = getmntent_r(f, &m, small, sizeof small);
	CHECK(r == &m);
	if (r)
		CHECK(!strcmp(m.mnt_fsname, "tmp") && m.mnt_passno == 1);
	CHECK(endmntent(f) == 1);

	/* A table in a file. */
	f = fopen("fstab", "w");
	CHECK(f && fputs("proc /proc proc rw,nosuid 0 0\n", f) >= 0 && fclose(f) == 0);
	f = setmntent("fstab", "r");
	CHECK(f);
	if (f) {
		r = getmntent(f);
		CHECK(r && !strcmp(r->mnt_opts, "rw,nosuid"));
		CHECK(!getmntent(f));
		CHECK(endmntent(f) == 1);
	}
	CHECK(endmntent(0) == 1);
	errno = 0;
	CHECK(!setmntent("missing", "r") && errno == ENOENT);

	/* The kernel's own table. */
	f = setmntent("/proc/self/mounts", "r");
	CHECK(f);
	count = 0;
	if (f) {
		while ((r = getmntent(f)))
			if (r->mnt_dir[0] == '/' && r->mnt_type[0])
				count++;
		CHECK(endmntent(f) == 1);
	}
	CHECK(count > 0);

	return t_status;
}
