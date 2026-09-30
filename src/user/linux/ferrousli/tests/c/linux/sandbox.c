/*
 * The calls bubblewrap 0.12 makes to build a sandbox as an unprivileged
 * user, made as its source makes them: capget and capset with each header
 * version, the prctl operations on capabilities, privileges, dumpability
 * and the parent's death, setns and unshare, the raw clone(2) through
 * syscall(2) it starts the sandbox with, and inside a new user namespace
 * the id maps, mount, pivot_root and umount2.
 *
 * Whether a user namespace may be made, and what may be done in it, is up to
 * the kernel and how it is configured: Linux may refuse one to an
 * unprivileged user outright (a sysctl), or make it and then deny the
 * capabilities it grants (Ubuntu's AppArmor restriction), and a Ferrix
 * kernel may not have them yet. Where the kernel refuses, the check is that
 * the refusal is one it gives; where it grants, the sandbox is built as
 * bwrap builds it. Every change is made in a child, so the test changes
 * nothing outside its directory whoever runs it, root included.
 */

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

/* linux/capability.h's, which neither musl's headers nor glibc's include;
 * bwrap takes them from there, and capget and capset from libc. */
#define CAP_V1 0x19980330u
#define CAP_V2 0x20071026u
#define CAP_V3 0x20080522u
#define CAP_SETPCAP 8
#define CAP_SYS_ADMIN 21
#define CAP_SYS_BOOT 22
struct cap_header {
	unsigned version;
	int pid;
};
struct cap_data {
	unsigned effective, permitted, inheritable;
};
int capget(struct cap_header *, struct cap_data *);
int capset(struct cap_header *, const struct cap_data *);

/* Declared by neither; bwrap makes it with syscall(2). */
int pivot_root(const char *, const char *);

/* linux/filter.h's and linux/seccomp.h's: an allow-everything filter. */
struct sock_filter {
	unsigned short code;
	unsigned char jt, jf;
	unsigned k;
};
struct sock_fprog {
	unsigned short len;
	struct sock_filter *filter;
};
#define BPF_RET_K 0x06
#define SECCOMP_MODE_FILTER 2
#define SECCOMP_RET_ALLOW 0x7fff0000u

#define SENTINEL 0x5a5a5a5au

static volatile sig_atomic_t signalled;

static void on_usr1(int sig)
{
	(void)sig;
	signalled = 1;
}

static void fill(struct cap_data *d, int n)
{
	for (int i = 0; i < n; i++)
		d[i].effective = d[i].permitted = d[i].inheritable = SENTINEL;
}

/* A refusal the kernel gives for a namespace it will not make. */
static int userns_refused(int error)
{
	return error == EPERM || error == EINVAL || error == ENOSPC || error == EUSERS
		|| error == ENOSYS || error == EACCES;
}

/* Runs `body` in a forked child, which exits with its t_status. */
static void in_child(void (*body)(void))
{
	int status;
	pid_t pid = fork();
	if (pid == 0) {
		t_status = 0;
		body();
		_exit(t_status);
	}
	CHECK(pid > 0);
	CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

/* Writes `text` to `name` under the directory `dir`, as bwrap's
 * write_file_at does: 0, or the error number. */
static int write_at(int dir, const char *name, const char *text)
{
	int fd = openat(dir, name, O_RDWR | O_CLOEXEC);
	if (fd < 0)
		return errno;
	ssize_t len = (ssize_t)strlen(text);
	int error = write(fd, text, (size_t)len) == len ? 0 : errno;
	close(fd);
	return error;
}

/* The overflow uid an unmapped id reads as, which bwrap reads too. */
static unsigned overflow_uid(void)
{
	char buf[16] = { 0 };
	int fd = open("/proc/sys/kernel/overflowuid", O_RDONLY | O_CLOEXEC);
	if (fd < 0)
		return 65534;
	ssize_t n = read(fd, buf, sizeof buf - 1);
	close(fd);
	return n > 0 ? (unsigned)strtoul(buf, NULL, 10) : 65534;
}

static void versions(void)
{
	struct cap_header h;
	struct cap_data d[3], one[2];
	int status;
	pid_t gone;

	/* Version 3, bwrap's: two structures, and the header as it was. */
	h = (struct cap_header){ CAP_V3, 0 };
	fill(d, 3);
	CHECK(capget(&h, d) == 0 && h.version == CAP_V3 && h.pid == 0);
	CHECK(d[0].permitted != SENTINEL && d[1].permitted != SENTINEL);
	CHECK(d[2].effective == SENTINEL);

	/* Version 1 has one, the low 32 capabilities; version 2 lays out two
	 * as 3 does. */
	h = (struct cap_header){ CAP_V1, 0 };
	fill(one, 2);
	CHECK(capget(&h, one) == 0 && h.version == CAP_V1);
	CHECK(one[0].permitted == d[0].permitted && one[1].permitted == SENTINEL);
	h = (struct cap_header){ CAP_V2, 0 };
	fill(one, 2);
	CHECK(capget(&h, one) == 0 && h.version == CAP_V2);
	CHECK(one[1].permitted == d[1].permitted);

	/* An unknown version is refused, and the kernel writes the one it
	 * prefers into the header; asked with no data, that is the answer. */
	h = (struct cap_header){ 0x12345678, 0 };
	errno = 0;
	CHECK(capget(&h, d) == -1 && errno == EINVAL && h.version == CAP_V3);
	h = (struct cap_header){ 0x12345678, 0 };
	CHECK(capget(&h, NULL) == 0 && h.version == CAP_V3);

	/* A process by its id: this one, a negative id, one that has gone. */
	h = (struct cap_header){ CAP_V3, getpid() };
	CHECK(capget(&h, one) == 0 && one[0].permitted == d[0].permitted);
	h = (struct cap_header){ CAP_V3, -1 };
	errno = 0;
	CHECK(capget(&h, one) == -1 && errno == EINVAL);
	gone = fork();
	if (gone == 0)
		_exit(0);
	CHECK(gone > 0 && waitpid(gone, &status, 0) == gone);
	h = (struct cap_header){ CAP_V3, gone };
	errno = 0;
	CHECK(capget(&h, one) == -1 && errno == ESRCH);
}

/* bwrap's drop_all_caps, and what capset refuses around it. */
static void drop_caps(void)
{
	struct cap_header h = { CAP_V3, 0 };
	struct cap_data d[2], none[2] = { { 0, 0, 0 }, { 0, 0, 0 } };

	CHECK(capget(&h, d) == 0);
	CHECK(capset(&h, d) == 0);
	h.pid = getpid();
	CHECK(capset(&h, d) == 0);
	h.pid = getppid();
	errno = 0;
	CHECK(capset(&h, d) == -1 && errno == EPERM);
	h.pid = 0;
	CHECK(capset(&h, none) == 0);
	CHECK(capget(&h, d) == 0);
	CHECK(d[0].permitted == 0 && d[1].permitted == 0 && d[0].effective == 0);
	/* What has been dropped cannot be taken back. */
	d[0].permitted = d[0].effective = 1u << CAP_SYS_ADMIN;
	errno = 0;
	CHECK(capset(&h, d) == -1 && errno == EPERM);
	/* Nor raised into the ambient set, now that it is not permitted. */
	if (prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, 0, 0, 0) >= 0) {
		errno = 0;
		CHECK(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, CAP_SYS_ADMIN, 0, 0) == -1
			&& errno == EPERM);
	}
}

/* The prctl operations bwrap and its libcap make, each with all four
 * arguments reaching the kernel: every one of these refuses a stray
 * nonzero argument, the fifth included. */
static void operations(void)
{
	struct cap_header h = { CAP_V3, 0 };
	struct cap_data d[2];
	int value, ret;

	/* The bounding set, read as libcap finds the last capability. */
	ret = prctl(PR_CAPBSET_READ, 0, 0, 0, 0);
	CHECK(ret == 0 || ret == 1);
	errno = 0;
	CHECK(prctl(PR_CAPBSET_READ, 64, 0, 0, 0) == -1 && errno == EINVAL);

	/* Dropping from it takes CAP_SETPCAP. */
	CHECK(capget(&h, d) == 0);
	ret = prctl(PR_CAPBSET_DROP, CAP_SYS_BOOT, 0, 0, 0);
	if (d[0].effective & (1u << CAP_SETPCAP))
		CHECK(ret == 0 && prctl(PR_CAPBSET_READ, CAP_SYS_BOOT, 0, 0, 0) == 0);
	else
		CHECK(ret == -1 && errno == EPERM);

	/* The ambient set, where the kernel has one. */
	ret = prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0);
	if (ret == 0) {
		CHECK(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, 0, 0, 0) == 0);
		errno = 0;
		CHECK(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, 0, 1, 0) == -1 && errno == EINVAL);
		errno = 0;
		CHECK(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, 0, 0, 1) == -1 && errno == EINVAL);
	} else {
		CHECK(errno == EINVAL);
	}

	/* No new privileges, and once set it stays. */
	errno = 0;
	CHECK(prctl(PR_SET_NO_NEW_PRIVS, 1, 1, 0, 0) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 1) == -1 && errno == EINVAL);
	CHECK(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0);
	CHECK(prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1);

	/* Keeping capabilities across a change of uid. */
	CHECK(prctl(PR_SET_KEEPCAPS, 1, 0, 0, 0) == 0 && prctl(PR_GET_KEEPCAPS, 0, 0, 0, 0) == 1);
	errno = 0;
	CHECK(prctl(PR_SET_KEEPCAPS, 2, 0, 0, 0) == -1 && errno == EINVAL);
	CHECK(prctl(PR_SET_KEEPCAPS, 0, 0, 0, 0) == 0 && prctl(PR_GET_KEEPCAPS, 0, 0, 0, 0) == 0);

	/* --die-with-parent. */
	value = 0;
	CHECK(prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0) == 0);
	CHECK(prctl(PR_GET_PDEATHSIG, &value, 0, 0, 0) == 0 && value == SIGKILL);
	errno = 0;
	CHECK(prctl(PR_SET_PDEATHSIG, 65, 0, 0, 0) == -1 && errno == EINVAL);
	CHECK(prctl(PR_SET_PDEATHSIG, 0, 0, 0, 0) == 0);

	/* Dumpable again once the privileges are gone, so /proc/self is the
	 * user's. */
	CHECK(prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) == 0 && prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) == 1);
	errno = 0;
	CHECK(prctl(PR_SET_DUMPABLE, 2, 0, 0, 0) == -1 && errno == EINVAL);

	/* The monitor's reaping of orphans. */
	value = 0;
	CHECK(prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == 0);
	CHECK(prctl(PR_GET_CHILD_SUBREAPER, &value, 0, 0, 0) == 0 && value == 1);

	/* --seccomp, called with bwrap's three arguments, where the kernel
	 * filters. No new privileges is set, so no privilege is needed. */
	{
		struct sock_filter allow = { BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW };
		struct sock_fprog program = { 1, &allow };
		ret = prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program);
		if (ret == 0)
			CHECK(prctl(PR_GET_SECCOMP, 0, 0, 0, 0) == SECCOMP_MODE_FILTER);
		else
			CHECK(errno == EINVAL);
	}
}

/* setns as bwrap's --userns and --pidns make it, where it must fail. */
static void join(void)
{
	int file, user, mnt;

	file = open("file", O_CREAT | O_RDWR | O_CLOEXEC, 0600);
	CHECK(file >= 0);
	errno = 0;
	CHECK(setns(file, 0) == -1 && errno == EINVAL);
	close(file);

	user = open("/proc/self/ns/user", O_RDONLY | O_CLOEXEC);
	mnt = open("/proc/self/ns/mnt", O_RDONLY | O_CLOEXEC);
	if (user >= 0) {
		/* Entering the namespace one is in would grant nothing. */
		errno = 0;
		CHECK(setns(user, CLONE_NEWUSER) == -1 && errno == EINVAL);
		close(user);
	}
	if (mnt >= 0) {
		/* A namespace of another type than asked for. */
		errno = 0;
		CHECK(setns(mnt, CLONE_NEWUSER) == -1 && errno == EINVAL);
		close(mnt);
	}
}

/* unshare(CLONE_NEWUSER), as bwrap's --userns2 path makes it: all
 * capabilities in the new namespace, and ids unmapped until a map is
 * written. */
static void unshare_user(void)
{
	struct cap_header h = { CAP_V3, 0 };
	struct cap_data d[2];
	int outer = open("/proc/self/ns/user", O_RDONLY | O_CLOEXEC);

	if (unshare(CLONE_NEWUSER) != 0) {
		CHECK(userns_refused(errno));
		return;
	}
	CHECK(getuid() == overflow_uid() && geteuid() == overflow_uid());
	/* Linux grants every capability in the new namespace, unless AppArmor
	 * has taken them away again. */
	CHECK(capget(&h, d) == 0);
	if (outer >= 0) {
		/* Back out into the parent's takes privilege over it. */
		errno = 0;
		CHECK(setns(outer, CLONE_NEWUSER) == -1 && errno == EPERM);
	}
}

/* The calling thread, found by the kernel's id for it: in a child of a raw
 * clone the thread's recorded id is its parent's. */
static void signal_self(void)
{
	signalled = 0;
	CHECK(pthread_kill(pthread_self(), SIGUSR1) == 0 && signalled);
	CHECK(gettid() == getpid());
}

/* The sandbox, as bwrap builds it for uid 1000 once its maps are written:
 * the old tree made a slave, a tmpfs root with the new root bound into it,
 * a pivot onto it, the old root detached, then a second pivot onto the new
 * root and the tmpfs detached, leaving an empty root. */
static void sandbox(unsigned uid, unsigned gid)
{
	struct cap_header h = { CAP_V3, 0 };
	struct cap_data d[2];
	char map[64];
	int self, error, old, entries;
	DIR *dir;

	signal_self();
	self = open("/proc/self", O_PATH | O_CLOEXEC);
	CHECK(self >= 0);
	snprintf(map, sizeof map, "%u %u 1\n", uid, uid);
	error = write_at(self, "uid_map", map);
	if (error != 0) {
		/* Made, but not the user's to map: the kernel's AppArmor
		 * restriction, which lets the namespace be made and denies the
		 * capabilities it grants. */
		CHECK(error == EPERM || error == EACCES);
		return;
	}
	error = write_at(self, "setgroups", "deny\n");
	CHECK(error == 0 || error == ENOENT);
	snprintf(map, sizeof map, "%u %u 1\n", gid, gid);
	CHECK(write_at(self, "gid_map", map) == 0);
	CHECK(getuid() == uid && getgid() == gid);
	CHECK(capget(&h, d) == 0 && (d[0].permitted & (1u << CAP_SYS_ADMIN)));
	CHECK(prctl(PR_CAPBSET_DROP, CAP_SYS_BOOT, 0, 0, 0) == 0);
	CHECK(prctl(PR_CAPBSET_READ, CAP_SYS_BOOT, 0, 0, 0) == 0);

	CHECK(mount(NULL, "/", NULL, MS_SILENT | MS_SLAVE | MS_REC, NULL) == 0);
	CHECK(mount("tmpfs", "base", "tmpfs", MS_NODEV | MS_NOSUID, NULL) == 0);
	CHECK(chdir("base") == 0);
	CHECK(mkdir("newroot", 0755) == 0);
	CHECK(mount("newroot", "newroot", NULL, MS_SILENT | MS_MGC_VAL | MS_BIND | MS_REC, NULL)
		== 0);
	CHECK(mkdir("oldroot", 0755) == 0);
	CHECK(pivot_root(".", "oldroot") == 0);
	CHECK(chdir("/") == 0);
	CHECK(mount("oldroot", "oldroot", NULL, MS_SILENT | MS_REC | MS_PRIVATE, NULL) == 0);
	CHECK(umount2("oldroot", MNT_DETACH) == 0);

	old = open("/", O_DIRECTORY | O_RDONLY | O_CLOEXEC);
	CHECK(old >= 0);
	CHECK(chdir("/newroot") == 0);
	CHECK(pivot_root(".", ".") == 0);
	CHECK(fchdir(old) == 0);
	CHECK(umount2(".", MNT_DETACH) == 0);
	CHECK(chdir("/") == 0);
	close(old);

	entries = 0;
	dir = opendir("/");
	CHECK(dir != NULL);
	if (dir) {
		struct dirent *e;
		while ((e = readdir(dir)))
			if (strcmp(e->d_name, ".") && strcmp(e->d_name, ".."))
				entries++;
		closedir(dir);
	}
	CHECK(entries == 0);

	/* bwrap's last drop, before it runs the program. */
	{
		struct cap_data none[2] = { { 0, 0, 0 }, { 0, 0, 0 } };
		CHECK(capset(&h, none) == 0);
		CHECK(prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) == 0);
	}
}

int main(void)
{
	int status, error;
	long pid;
	unsigned uid = getuid(), gid = getgid();

	CHECK(signal(SIGUSR1, on_usr1) != SIG_ERR);
	CHECK(mkdir("base", 0755) == 0);

	versions();
	in_child(drop_caps);
	in_child(operations);
	join();
	in_child(unshare_user);

	/* Privilege is checked before the paths are looked up. */
	errno = 0;
	CHECK(pivot_root("missing", "missing") == -1 && (errno == EPERM || errno == ENOENT));

	/* A raw clone, as bwrap starts its sandbox with: the child returns
	 * from syscall(2) on a copy of the parent's stack. */
	pid = syscall(SYS_clone, SIGCHLD, 0);
	if (pid == 0) {
		t_status = 0;
		signal_self();
		_exit(t_status);
	}
	CHECK(pid > 0);
	CHECK(waitpid((pid_t)pid, &status, 0) == pid && WIFEXITED(status)
		&& WEXITSTATUS(status) == 0);

	/* And with bwrap's namespaces. */
	pid = syscall(SYS_clone, SIGCHLD | CLONE_NEWNS | CLONE_NEWUSER, 0);
	if (pid == 0) {
		t_status = 0;
		sandbox(uid, gid);
		_exit(t_status);
	}
	error = errno;
	if (pid == -1) {
		CHECK(userns_refused(error));
	} else {
		CHECK(waitpid((pid_t)pid, &status, 0) == pid && WIFEXITED(status)
			&& WEXITSTATUS(status) == 0);
	}
	return t_status;
}
