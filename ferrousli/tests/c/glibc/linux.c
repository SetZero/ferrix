/*
 * The Linux calls glibc wraps that systemd, GLib, libmount and Chrome
 * link against: statx, the process descriptors, close_range and closefrom,
 * mincore, ptrace's peek, clone, recvmmsg and sendmmsg, the new mount API
 * and file handles, a message queue's attributes, ftok, getdtablesize and
 * get_current_dir_name.
 *
 * The mount calls need privilege this test does not have, and a kernel may
 * lack the newest calls; for those the check is that the call reached the
 * kernel and came back with one of the answers the kernel gives, not
 * EFAULT or EINVAL from arguments passed wrongly.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

int pidfd_open(pid_t, unsigned int);
int pidfd_send_signal(int, int, siginfo_t *, unsigned int);
int close_range(unsigned int, unsigned int, int);
void closefrom(int);
int open_tree(int, const char *, unsigned int);
int move_mount(int, const char *, int, const char *, unsigned int);
int mount_setattr(int, const char *, unsigned int, void *, size_t);

static volatile long peeked = 0x5eed1234;

static int child(void *arg)
{
	return *(int *)arg + 1;
}

/* An answer the kernel gives, rather than one from a malformed call. */
static int kernel_refused(int ret)
{
	return ret == -1 && (errno == EPERM || errno == ENOSYS || errno == EOPNOTSUPP
		|| errno == EACCES);
}

int main(void)
{
	struct statx sx;
	struct stat st;
	struct rlimit rl;
	int fd, pidfd, status, sv[2];
	pid_t pid;

	fd = open("file", O_CREAT | O_RDWR, 0640);
	CHECK(fd >= 0 && write(fd, "twelve bytes", 12) == 12);

	/* statx against fstat. */
	CHECK(statx(AT_FDCWD, "file", 0, STATX_BASIC_STATS, &sx) == 0);
	CHECK(fstat(fd, &st) == 0);
	CHECK(sx.stx_size == 12 && sx.stx_ino == st.st_ino && (sx.stx_mode & 07777) == 0640);
	CHECK(statx(fd, "", AT_EMPTY_PATH, STATX_SIZE, &sx) == 0 && sx.stx_size == 12);
	errno = 0;
	CHECK(statx(AT_FDCWD, "missing", 0, STATX_BASIC_STATS, &sx) == -1 && errno == ENOENT);

	/* A process descriptor becomes readable when the child exits, and
	 * pidfd_send_signal kills one that would not. */
	pid = fork();
	if (pid == 0) {
		pause();
		_exit(0);
	}
	pidfd = pidfd_open(pid, 0);
	if (pidfd >= 0) {
		struct pollfd p = { pidfd, POLLIN, 0 };
		CHECK(poll(&p, 1, 0) == 0);
		CHECK(pidfd_send_signal(pidfd, SIGKILL, NULL, 0) == 0);
		CHECK(poll(&p, 1, 5000) == 1 && (p.revents & POLLIN));
		close(pidfd);
	} else {
		CHECK(errno == ENOSYS);
		kill(pid, SIGKILL);
	}
	CHECK(waitpid(pid, &status, 0) == pid && WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL);

	/* close_range closes a range; closefrom everything from a number up. */
	CHECK(dup2(fd, 40) == 40 && dup2(fd, 41) == 41 && dup2(fd, 50) == 50);
	if (close_range(40, 41, 0) == 0) {
		CHECK(fcntl(40, F_GETFD) == -1 && fcntl(41, F_GETFD) == -1 && fcntl(50, F_GETFD) == 0);
	} else {
		CHECK(errno == ENOSYS);
	}
	closefrom(45);
	CHECK(fcntl(50, F_GETFD) == -1 && fcntl(fd, F_GETFD) == 0);

	/* mincore: a page just written is resident. */
	long page = sysconf(_SC_PAGESIZE);
	unsigned char *map = mmap(NULL, page * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	unsigned char vec[2] = { 0xff, 0xff };
	CHECK(map != MAP_FAILED);
	map[0] = 1;
	CHECK(mincore(map, page * 2, vec) == 0 && (vec[0] & 1));
	errno = 0;
	CHECK(mincore(map + 1, page, vec) == -1 && errno == EINVAL);
	munmap(map, page * 2);

	/* ptrace's peek returns the word, and clears errno. */
	pid = fork();
	if (pid == 0) {
		if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0)
			_exit(3);
		raise(SIGSTOP);
		_exit(0);
	}
	CHECK(waitpid(pid, &status, 0) == pid);
	if (WIFSTOPPED(status)) {
		errno = 1;
		CHECK(ptrace(PTRACE_PEEKDATA, pid, (void *)&peeked, NULL) == 0x5eed1234 && errno == 0);
		CHECK(ptrace(PTRACE_CONT, pid, NULL, NULL) == 0);
		CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
	} else {
		/* No ptrace here: the child could not trace itself. */
		CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 3);
	}

	/* clone like fork, running a function on a stack of its own. */
	{
		size_t size = 64 * 1024;
		char *stack = malloc(size);
		int arg = 6;
		CHECK(stack != NULL);
		pid = clone(child, stack + size, SIGCHLD, &arg);
		CHECK(pid > 0);
		CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 7);
		errno = 0;
		CHECK(clone(child, NULL, SIGCHLD, &arg) == -1 && errno == EINVAL);
		free(stack);
	}

	/* Two datagrams out with sendmmsg, both in with one recvmmsg. */
	CHECK(socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) == 0);
	{
		char a[] = "first", b[] = "second", ra[16], rb[16];
		struct iovec out[2] = { { a, 5 }, { b, 6 } }, in[2] = { { ra, 16 }, { rb, 16 } };
		struct mmsghdr send[2], recv[2];
		struct timespec wait = { 5, 0 };
		memset(send, 0, sizeof send);
		memset(recv, 0, sizeof recv);
		send[0].msg_hdr.msg_iov = &out[0];
		send[0].msg_hdr.msg_iovlen = 1;
		send[1].msg_hdr.msg_iov = &out[1];
		send[1].msg_hdr.msg_iovlen = 1;
		recv[0].msg_hdr.msg_iov = &in[0];
		recv[0].msg_hdr.msg_iovlen = 1;
		recv[1].msg_hdr.msg_iov = &in[1];
		recv[1].msg_hdr.msg_iovlen = 1;
		CHECK(sendmmsg(sv[0], send, 2, 0) == 2 && send[0].msg_len == 5 && send[1].msg_len == 6);
		CHECK(recvmmsg(sv[1], recv, 2, MSG_WAITFORONE, &wait) == 2);
		CHECK(recv[0].msg_len == 5 && memcmp(ra, "first", 5) == 0);
		CHECK(recv[1].msg_len == 6 && memcmp(rb, "second", 6) == 0);
		close(sv[0]);
		close(sv[1]);
	}

	/* The mount API and file handles, as far as an unprivileged caller
	 * gets: open_tree without a copy is an O_PATH open. */
	{
		int tree = open_tree(AT_FDCWD, ".", 0x80000 /* OPEN_TREE_CLOEXEC */);
		CHECK(tree >= 0 || kernel_refused(tree));
		if (tree >= 0) {
			CHECK(fstat(tree, &st) == 0 && S_ISDIR(st.st_mode));
			close(tree);
		}
		CHECK(kernel_refused(move_mount(AT_FDCWD, "file", AT_FDCWD, "elsewhere", 0))
			|| (errno == ENOENT || errno == EINVAL));
		char attr[32] = { 0 };
		int ret = mount_setattr(AT_FDCWD, ".", 0, attr, sizeof attr);
		CHECK(ret == 0 || kernel_refused(ret) || errno == EINVAL);

		struct { struct file_handle h; unsigned char bytes[128]; } handle;
		int mount_id;
		handle.h.handle_bytes = 128;
		ret = name_to_handle_at(AT_FDCWD, "file", &handle.h, &mount_id, 0);
		CHECK(ret == 0 || kernel_refused(ret));
		handle.h.handle_bytes = 0;
		errno = 0;
		CHECK(name_to_handle_at(AT_FDCWD, "file", &handle.h, &mount_id, 0) == -1
			&& (errno == EOVERFLOW || errno == EOPNOTSUPP || errno == ENOSYS));
	}

	/* A descriptor that is not a message queue. */
	{
		struct mq_attr attr;
		errno = 0;
		CHECK(mq_getattr(fd, &attr) == -1 && (errno == EBADF || errno == ENOSYS));
	}

	/* ftok: the same key for the same file and id, another for another id. */
	CHECK(ftok("file", 'a') != -1 && ftok("file", 'a') == ftok("file", 'a'));
	CHECK(ftok("file", 'a') != ftok("file", 'b'));
	CHECK(((unsigned)ftok("file", 'a') >> 24) == 'a');
	CHECK(ftok("missing", 'a') == -1);

	CHECK(getrlimit(RLIMIT_NOFILE, &rl) == 0);
	CHECK(getdtablesize() == (rl.rlim_cur > INT32_MAX ? INT32_MAX : (int)rl.rlim_cur));

	/* get_current_dir_name: $PWD when it names the directory, getcwd's
	 * answer when it does not. */
	{
		char cwd[4096], *name;
		CHECK(getcwd(cwd, sizeof cwd) != NULL);
		CHECK(setenv("PWD", "/", 1) == 0);
		name = get_current_dir_name();
		CHECK(name != NULL && strcmp(name, cwd) == 0);
		free(name);
		CHECK(symlink(".", "here") == 0);
		char via[4200];
		strcpy(via, cwd);
		strcat(via, "/here");
		CHECK(setenv("PWD", via, 1) == 0);
		name = get_current_dir_name();
		CHECK(name != NULL && strcmp(name, via) == 0);
		free(name);
	}

	close(fd);
	return t_status;
}
