/*
 * File system statistics, and the calls that mount, unmount, swap, change the
 * root and sync.
 *
 * The test must change nothing outside its directory whoever runs it. So the
 * mount calls are made where the kernel fails them before it checks for
 * privilege, and the calls that check for privilege first are made on a path
 * that does not exist, failing either way.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/statfs.h>
#include <sys/statvfs.h>
#include <sys/swap.h>
#include <unistd.h>

#include "check.h"

/* musl's headers do not declare it. */
int pivot_root(const char *, const char *);

/* PROC_SUPER_MAGIC, from linux/magic.h. */
#define PROC_MAGIC 0x9fa0

int main(void)
{
	struct statfs sf, fsf;
	struct statvfs sv, fsv;
	char block[8192];
	int fd, p[2];

	fd = open("file", O_RDWR | O_CREAT, 0644);
	CHECK(fd >= 0);
	memset(block, 'x', sizeof block);
	CHECK(write(fd, block, sizeof block) == sizeof block);

	/* statfs and statvfs describe the same file system, by path and by
	 * descriptor. */
	CHECK(statfs(".", &sf) == 0 && fstatfs(fd, &fsf) == 0);
	CHECK(sf.f_type == fsf.f_type && sf.f_bsize == fsf.f_bsize);
	CHECK(sf.f_blocks == fsf.f_blocks && sf.f_namelen == fsf.f_namelen);
	CHECK(sf.f_bsize > 0 && sf.f_namelen > 0);
	CHECK(statvfs(".", &sv) == 0 && fstatvfs(fd, &fsv) == 0);
	CHECK(sv.f_bsize == sf.f_bsize && sv.f_blocks == sf.f_blocks);
	CHECK(sv.f_frsize == (sf.f_frsize ? sf.f_frsize : sf.f_bsize));
	CHECK(sv.f_files == sf.f_files && sv.f_favail == sv.f_ffree);
	CHECK(sv.f_namemax == sf.f_namelen && sv.f_type == sf.f_type);
	CHECK(sv.f_fsid == fsv.f_fsid && sv.f_flag == fsv.f_flag);
	errno = 0;
	CHECK(statfs("missing", &sf) == -1 && errno == ENOENT);
	errno = 0;
	CHECK(fstatvfs(-1, &sv) == -1 && errno == EBADF);

	/* A field past the middle of each structure, by a value it must hold. */
	CHECK(statfs("/proc", &sf) == 0 && sf.f_type == PROC_MAGIC);
	CHECK(statvfs("/proc", &sv) == 0 && sv.f_type == PROC_MAGIC);
	CHECK(sv.f_namemax == sf.f_namelen && sv.f_flag == sf.f_flags);

	/* sync, syncfs and readahead. */
	sync();
	CHECK(syncfs(fd) == 0);
	errno = 0;
	CHECK(syncfs(-1) == -1 && errno == EBADF);
	CHECK(readahead(fd, 0, sizeof block) == 0);
	CHECK(pipe(p) == 0);
	errno = 0;
	CHECK(readahead(p[0], 0, 1) == -1 && errno == EINVAL);

	/* The kernel resolves these paths, or copies the type, before it asks
	 * for privilege. */
	errno = 0;
	CHECK(mount("none", "missing", "tmpfs", 0, "size=1m") == -1 && errno == ENOENT);
	errno = 0;
	CHECK(mount("none", ".", (const char *)1, 0, 0) == -1 && errno == EFAULT);
	errno = 0;
	CHECK(umount("missing") == -1 && errno == ENOENT);
	errno = 0;
	CHECK(umount2(".", 0x40000000) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(chroot("missing") == -1 && errno == ENOENT);
	errno = 0;
	CHECK(chroot("file") == -1 && errno == ENOTDIR);

	/* These ask for privilege first. */
	errno = 0;
	CHECK(swapon("missing", 0) == -1 && (errno == EPERM || errno == ENOENT));
	errno = 0;
	CHECK(swapoff("missing") == -1 && (errno == EPERM || errno == ENOENT));
	errno = 0;
	CHECK(pivot_root("missing", "missing") == -1 && (errno == EPERM || errno == ENOENT));

	return t_status;
}
