/*
 * The Linux and process calls busybox needs beyond POSIX's core: prctl,
 * capabilities, personality, namespaces, scheduling, inotify, sendfile,
 * flock, sysinfo, klogctl and reboot, ids, host names, ownership, file
 * times, interval timers and the clock's calls.
 *
 * The test must change nothing outside its directory whoever runs it, root
 * included. So each call that needs privilege is made where it fails before
 * or after its privilege check alike: an unknown reboot command, a host name
 * too long, a clock time with too many microseconds.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/inotify.h>
#include <sys/klog.h>
#include <sys/personality.h>
#include <sys/prctl.h>
#include <sys/reboot.h>
#include <sys/sendfile.h>
#include <sys/stat.h>
#include <sys/sysinfo.h>
#include <sys/time.h>
#include <sys/timex.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

/* musl's headers do not declare them; libcap's do. */
struct cap_header {
	unsigned version;
	int pid;
};
int capget(struct cap_header *, void *);
int capset(struct cap_header *, const void *);

int main(void)
{
	char name[16], buf[64], long_name[65];
	struct cap_header header = { 0, 0 };
	cpu_set_t set;
	int notify, watch, source, dest, other, size;
	off_t offset;
	struct sysinfo info;
	double loads[5];
	struct itimerval timer, old;
	struct timex tx;
	uid_t ruid, euid, suid;
	gid_t rgid, egid, sgid;
	struct stat st;
	struct timeval times[2];

	/* prctl: the thread's name, set and read. */
	CHECK(prctl(PR_SET_NAME, "ferrousli-test", 0, 0, 0) == 0);
	CHECK(prctl(PR_GET_NAME, name) == 0 && !strcmp(name, "ferrousli-test"));

	/* capget without data, for a version the kernel does not know, stores
	 * the version it prefers; capset refuses that version. */
	CHECK(capget(&header, 0) == 0 && header.version == 0x20080522);
	header.version = 0;
	errno = 0;
	CHECK(capset(&header, buf) == -1 && errno == EINVAL);

	/* The persona read without a change, namespaces left as they are. */
	CHECK(personality(0xffffffff) >= 0);
	CHECK(unshare(0) == 0);
	errno = 0;
	CHECK(setns(-1, 0) == -1 && errno == EBADF);

	/* Scheduling. */
	CHECK(sched_yield() == 0);
	CPU_ZERO(&set);
	CHECK(sched_getaffinity(0, sizeof set, &set) == 0 && CPU_COUNT(&set) > 0);
	CHECK(sched_setaffinity(0, sizeof set, &set) == 0);

	/* inotify sees a file created in the directory. */
	notify = inotify_init1(IN_CLOEXEC | IN_NONBLOCK);
	CHECK(notify >= 0 && (fcntl(notify, F_GETFD) & FD_CLOEXEC));
	watch = inotify_add_watch(notify, ".", IN_CREATE);
	CHECK(watch >= 0);
	source = open("source", O_RDWR | O_CREAT, 0600);
	CHECK(source >= 0 && write(source, "0123456789", 10) == 10);
	CHECK(read(notify, buf, sizeof buf) > 0);
	CHECK(inotify_rm_watch(notify, watch) == 0);
	close(notify);
	notify = inotify_init();
	CHECK(notify >= 0);
	close(notify);

	/* sendfile from an offset, which it advances. */
	dest = open("dest", O_RDWR | O_CREAT, 0600);
	offset = 4;
	CHECK(sendfile(dest, source, &offset, 3) == 3 && offset == 7);
	CHECK(pread(dest, buf, 3, 0) == 3 && !memcmp(buf, "456", 3));

	/* flock: an exclusive lock another open file cannot share. */
	CHECK(flock(source, LOCK_EX) == 0);
	other = open("source", O_RDONLY);
	errno = 0;
	CHECK(flock(other, LOCK_SH | LOCK_NB) == -1 && errno == EWOULDBLOCK);
	CHECK(flock(source, LOCK_UN) == 0 && flock(other, LOCK_SH | LOCK_NB) == 0);
	close(other);

	/* sysinfo, the kernel log's size, and a reboot command that does not
	 * exist. */
	CHECK(sysinfo(&info) == 0 && info.totalram > 0 && info.mem_unit > 0 && info.procs > 0);
	/* getloadavg: up to three averages, none for n of zero, -1 for a negative n. */
	CHECK(getloadavg(loads, 5) == 3 && loads[0] >= 0 && loads[1] >= 0 && loads[2] >= 0);
	CHECK(getloadavg(loads, 0) == 0 && getloadavg(loads, -1) == -1);
	errno = 0;
	size = klogctl(10, 0, 0);
	CHECK(size > 0 || errno == EPERM);
	errno = 0;
	CHECK(reboot(0x12345678) == -1 && (errno == EPERM || errno == EINVAL));

	/* Ids and names. */
	CHECK(getresuid(&ruid, &euid, &suid) == 0 && ruid == getuid() && euid == geteuid());
	CHECK(getresgid(&rgid, &egid, &sgid) == 0 && rgid == getgid() && egid == getegid());
	CHECK(setpgrp() == 0 && getpgrp() == getpid());
	CHECK(gethostid() == 0);
	memset(long_name, 'x', sizeof long_name);
	errno = 0;
	CHECK(sethostname(long_name, sizeof long_name) == -1 && (errno == EPERM || errno == EINVAL));
	errno = 0;
	CHECK(setdomainname(long_name, sizeof long_name) == -1 && (errno == EPERM || errno == EINVAL));

	/* Ownership, set to what the file already has. */
	CHECK(stat("source", &st) == 0);
	CHECK(chown("source", st.st_uid, st.st_gid) == 0);
	CHECK(fchown(source, -1, -1) == 0);
	CHECK(fchownat(AT_FDCWD, "source", st.st_uid, -1, 0) == 0);
	CHECK(symlink("source", "link") == 0 && lchown("link", st.st_uid, -1) == 0);
	errno = 0;
	CHECK(chown("missing", 0, 0) == -1 && errno == ENOENT);

	/* File times in microseconds. */
	times[0].tv_sec = 1000000000;
	times[0].tv_usec = 500000;
	times[1].tv_sec = 1234567890;
	times[1].tv_usec = 0;
	CHECK(utimes("source", times) == 0 && stat("source", &st) == 0);
	CHECK(st.st_atim.tv_sec == 1000000000 && st.st_atim.tv_nsec == 500000000);
	CHECK(st.st_mtim.tv_sec == 1234567890 && st.st_mtim.tv_nsec == 0);
	times[0].tv_usec = 1000000;
	errno = 0;
	CHECK(utimes("source", times) == -1 && errno == EINVAL);
	CHECK(utimes("source", 0) == 0);

	/* fallocate: storage for a range, and a mode the kernel refuses. */
	CHECK(fallocate(source, 0, 0, 4096) == 0);
	CHECK(fstat(source, &st) == 0 && st.st_size >= 4096);
	errno = 0;
	CHECK(fallocate(source, 0, -1, 1) == -1 && errno == EINVAL);

	/* The effective ids' access check, under both its names. */
	CHECK(euidaccess("source", R_OK | W_OK) == 0);
	CHECK(eaccess("source", F_OK) == 0);
	errno = 0;
	CHECK(eaccess("missing", F_OK) == -1 && errno == ENOENT);

	/* An interval timer, set, read back, and cancelled. */
	memset(&timer, 0, sizeof timer);
	timer.it_value.tv_sec = 100;
	CHECK(setitimer(ITIMER_REAL, &timer, 0) == 0);
	CHECK(getitimer(ITIMER_REAL, &old) == 0);
	CHECK(old.it_value.tv_sec >= 90 && old.it_value.tv_sec <= 100);
	memset(&timer, 0, sizeof timer);
	CHECK(setitimer(ITIMER_REAL, &timer, &old) == 0 && old.it_value.tv_sec >= 90);

	/* The clock: nothing given to set, a microsecond count out of range,
	 * and its state read. */
	CHECK(settimeofday(0, 0) == 0);
	times[0].tv_sec = 0;
	times[0].tv_usec = 1000000;
	errno = 0;
	CHECK(settimeofday(&times[0], 0) == -1 && errno == EINVAL);
	memset(&tx, 0, sizeof tx);
	CHECK(adjtimex(&tx) >= 0);
	memset(&tx, 0, sizeof tx);
	CHECK(clock_adjtime(CLOCK_REALTIME, &tx) >= 0);

	close(source);
	close(dest);
	return t_status;
}
