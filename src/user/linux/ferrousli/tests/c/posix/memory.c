/*
 * Memory: anonymous mappings, protection and a child that faults on it,
 * mremap, a shared mapping of a memfd, advice and locking, and the errors.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	size_t page = sysconf(_SC_PAGESIZE);
	unsigned char *m, *grown, *second;
	char buf[8];
	int status, fd, r;
	pid_t pid;

	/* An anonymous mapping, zeroed, read and written, then unmapped. */
	m = mmap(0, 3 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK(m != MAP_FAILED);
	if (m == MAP_FAILED)
		return t_status;
	CHECK(m[0] == 0 && m[3 * page - 1] == 0);
	memset(m, 0xa5, 3 * page);
	CHECK(m[page + 1] == 0xa5 && m[3 * page - 1] == 0xa5);

	/* mremap grows the mapping, keeping its contents, and shrinks it. */
	grown = mremap(m, 3 * page, 64 * page, MREMAP_MAYMOVE);
	CHECK(grown != MAP_FAILED);
	if (grown == MAP_FAILED)
		return t_status;
	CHECK(grown[0] == 0xa5 && grown[3 * page - 1] == 0xa5 && grown[63 * page] == 0);
	grown[63 * page] = 1;
	CHECK(mremap(grown, 64 * page, page, 0) == grown);

	/*
	 * Advice, syncing and locking, on the one page left. Advice or a lock on
	 * part of a mapping splits it, and mremap refuses a range that spans the
	 * pieces, so this comes after the growing.
	 */
	CHECK(msync(grown, page, MS_ASYNC) == 0);
	CHECK(madvise(grown, page, MADV_WILLNEED) == 0);
	/* POSIX_MADV_DONTNEED must not discard the contents. */
	CHECK(posix_madvise(grown, page, POSIX_MADV_DONTNEED) == 0 && grown[0] == 0xa5);
	CHECK(posix_madvise(grown, page, POSIX_MADV_SEQUENTIAL) == 0);
	errno = 0;
	CHECK(posix_madvise(grown, page, 12345) == EINVAL && errno == 0);
	errno = 0;
	r = mlock(grown, page);
	CHECK(r == 0 || errno == ENOMEM || errno == EPERM);
	if (r == 0)
		CHECK(munlock(grown, page) == 0);
	CHECK(munlockall() == 0);
	errno = 0;
	CHECK(mlockall(0) == -1 && errno == EINVAL);

	errno = 0;
	CHECK(mremap(grown, page, SIZE_MAX, MREMAP_MAYMOVE) == MAP_FAILED && errno == ENOMEM);
	errno = 0;
	CHECK(mremap(grown, page, 2 * page, 0x100) == MAP_FAILED && errno == EINVAL);
	CHECK(munmap(grown, page) == 0);

	/* mmap's errors. */
	errno = 0;
	CHECK(mmap(0, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED && errno == EINVAL);
	errno = 0;
	CHECK(mmap(0, page, PROT_READ, MAP_PRIVATE, -1, 0) == MAP_FAILED && errno == EBADF);
	errno = 0;
	CHECK(mmap(0, SIZE_MAX, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED && errno == ENOMEM);
	fd = open("file", O_RDWR | O_CREAT | O_EXCL, 0600);
	CHECK(fd >= 0 && ftruncate(fd, page) == 0);
	errno = 0;
	CHECK(mmap(0, page, PROT_READ, MAP_SHARED, fd, 1) == MAP_FAILED && errno == EINVAL);
	CHECK(close(fd) == 0);
	errno = 0;
	CHECK(munmap((void *)1, page) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(mprotect((void *)1, page, PROT_READ) == -1 && errno == EINVAL);

	/* A forked child writes after mprotect(PROT_READ) and dies of SIGSEGV. */
	m = mmap(0, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK(m != MAP_FAILED);
	if (m == MAP_FAILED)
		return t_status;
	pid = fork();
	if (pid == 0) {
		struct rlimit none = { 0, 0 };
		volatile unsigned char *v = m;
		setrlimit(RLIMIT_CORE, &none);
		v[0] = 1;
		if (mprotect(m, page, PROT_READ) != 0)
			_exit(1);
		if (v[0] != 1)
			_exit(2);
		v[0] = 2;
		_exit(3);
	}
	CHECK(waitpid(pid, &status, 0) == pid);
	CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV);
	/* The parent's private copy was never touched, and is still writable. */
	CHECK(m[0] == 0);
	m[0] = 5;
	CHECK(munmap(m, page) == 0);

	/* memfd_create, ftruncate, then a shared mapping. */
	fd = memfd_create("ferrousli", MFD_CLOEXEC);
	CHECK(fd >= 0 && fcntl(fd, F_GETFD) == FD_CLOEXEC);
	CHECK(ftruncate(fd, 2 * page) == 0);
	m = mmap(0, 2 * page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	CHECK(m != MAP_FAILED);
	if (m == MAP_FAILED)
		return t_status;
	memcpy(m + page, "shared", 6);
	CHECK(pread(fd, buf, 6, page) == 6 && memcmp(buf, "shared", 6) == 0);
	CHECK(pwrite(fd, "S", 1, page) == 1 && m[page] == 'S');
	/* A second mapping at an offset sees the same pages. */
	second = mmap(0, page, PROT_READ, MAP_SHARED, fd, page);
	CHECK(second != MAP_FAILED && second[0] == 'S');
	/* A child's writes reach the parent through the shared mapping. */
	pid = fork();
	if (pid == 0) {
		m[0] = 'c';
		_exit(0);
	}
	CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status));
	CHECK(m[0] == 'c');
	CHECK(msync(m, 2 * page, MS_SYNC) == 0);
	CHECK(munmap(second, page) == 0 && munmap(m, 2 * page) == 0);
	CHECK(close(fd) == 0);
	errno = 0;
	CHECK(memfd_create("bad", 0x10000) == -1 && errno == EINVAL);

	return t_status;
}
