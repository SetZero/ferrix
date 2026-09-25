/*
 * What the libraries of Chrome's window build import from glibc beyond the
 * names the headless shell needs: BSD's err and warn families, the terminal
 * table, lockf, __strlcpy_chk, GNU's obstacks, the rest of the new mount
 * API, and the reader-writer lock's kind -- libblkid and libmount, CUPS,
 * GMP, GnuTLS and libunistring call them.
 *
 * With "strlcpy" as its argument the program asks __strlcpy_chk for more than
 * the object holds, which must abort before writing.
 */

#define _GNU_SOURCE
#include <err.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

extern char *program_invocation_short_name;

size_t __strlcpy_chk(char *, const char *, size_t, size_t);
size_t __strlcat_chk(char *, const char *, size_t, size_t);

struct ttyent;
struct ttyent *getttyent(void);
struct ttyent *getttynam(const char *);
int setttyent(void);
int endttyent(void);

int fsopen(const char *, unsigned int);
int fsconfig(int, unsigned int, const char *, const void *, int);
int fsmount(int, unsigned int, unsigned int);
int fspick(int, const char *, unsigned int);

int pthread_rwlockattr_setkind_np(pthread_rwlockattr_t *, int);
int pthread_rwlockattr_getkind_np(const pthread_rwlockattr_t *, int *);

/* glibc's struct obstack, as its first interface lays it out. */
struct chunk {
	char *limit;
	struct chunk *prev;
	char contents[];
};
struct obstack {
	long chunk_size;
	struct chunk *chunk;
	char *object_base;
	char *next_free;
	char *chunk_limit;
	union { long i; void *p; } temp;
	int alignment_mask;
	void *(*chunkfun)(long);
	void (*freefun)(void *);
	void *extra_arg;
	unsigned use_extra_arg : 1, maybe_empty_object : 1, alloc_failed : 1;
};
int _obstack_begin(struct obstack *, int, int, void *(*)(long), void (*)(void *));
void _obstack_newchunk(struct obstack *, int);
void obstack_free(struct obstack *, void *);
int _obstack_memory_used(struct obstack *);
int _obstack_allocated_p(struct obstack *, void *);
int obstack_printf(struct obstack *, const char *, ...);

static void *chunk_alloc(long size) { return malloc(size); }

/* obstack_grow and obstack_finish, as glibc's header expands them. */
static void grow(struct obstack *h, const void *bytes, int n)
{
	if (h->chunk_limit - h->next_free < n)
		_obstack_newchunk(h, n);
	memcpy(h->next_free, bytes, n);
	h->next_free += n;
}

static void *finish(struct obstack *h)
{
	void *object = h->object_base;
	unsigned long mask = h->alignment_mask;
	h->next_free = (char *)(((unsigned long)h->next_free + mask) & ~mask);
	if (h->next_free > h->chunk_limit)
		h->next_free = h->chunk_limit;
	h->object_base = h->next_free;
	return object;
}

/* What standard error received while `write` ran. */
static void captured(void (*write)(void), char *out, size_t size)
{
	FILE *file = tmpfile();
	CHECK(file != NULL);
	if (!file)
		return;
	int saved = dup(2);
	dup2(fileno(file), 2);
	write();
	dup2(saved, 2);
	close(saved);
	size_t n = pread(fileno(file), out, size - 1, 0);
	out[n == (size_t)-1 ? 0 : n] = 0;
	fclose(file);
}

static void warnings(void)
{
	errno = ENOENT;
	warn("opening %s", "x");
	warnx("%d left", 3);
	errno = EACCES;
	warn(NULL);
}

static int kernel_refused(int ret)
{
	return ret == -1 && (errno == EPERM || errno == ENOSYS || errno == EOPNOTSUPP
		|| errno == EACCES);
}

int main(int argc, char **argv)
{
	if (argc > 1 && strcmp(argv[1], "strlcpy") == 0) {
		char small[4];
		__strlcpy_chk(small, "far too long", 16, sizeof small);
		return 0;
	}

	/* warn and warnx: the program's name in front, errno's text after. */
	{
		char out[512], want[512];
		const char *name = program_invocation_short_name;
		captured(warnings, out, sizeof out);
		snprintf(want, sizeof want, "%s: opening x: %s\n%s: 3 left\n%s: %s\n",
			name, strerror(ENOENT), name, name, strerror(EACCES));
		CHECK(strcmp(out, want) == 0);
	}

	/* errx exits with its status. */
	{
		pid_t child = fork();
		if (child == 0) {
			close(2);
			errx(3, "bye");
		}
		int status = 0;
		CHECK(waitpid(child, &status, 0) == child);
		CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 3);
	}

	/* lockf: a lock another process holds is seen, and taken back. */
	{
		int fd = open("locked", O_RDWR | O_CREAT, 0600);
		CHECK(fd >= 0);
		CHECK(lockf(fd, F_TLOCK, 0) == 0);
		CHECK(lockf(fd, F_TEST, 0) == 0);
		pid_t child = fork();
		if (child == 0) {
			int other = open("locked", O_RDWR);
			int held = lockf(other, F_TEST, 0) == -1 && errno == EACCES;
			int refused = lockf(other, F_TLOCK, 0) == -1
				&& (errno == EACCES || errno == EAGAIN);
			_exit(held && refused ? 0 : 1);
		}
		int status = 1;
		CHECK(waitpid(child, &status, 0) == child);
		CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
		CHECK(lockf(fd, F_ULOCK, 0) == 0);
		child = fork();
		if (child == 0) {
			int other = open("locked", O_RDWR);
			_exit(lockf(other, F_TEST, 0) == 0 ? 0 : 1);
		}
		CHECK(waitpid(child, &status, 0) == child);
		CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
		errno = 0;
		CHECK(lockf(fd, 9, 0) == -1 && errno == EINVAL);
		close(fd);
	}

	/* __strlcpy_chk and __strlcat_chk, within the object. */
	{
		char buf[8];
		CHECK(__strlcpy_chk(buf, "abcdefghij", sizeof buf, sizeof buf) == 10);
		CHECK(strcmp(buf, "abcdefg") == 0);
		CHECK(__strlcpy_chk(buf, "ab", sizeof buf, sizeof buf) == 2);
		CHECK(__strlcat_chk(buf, "cd", sizeof buf, sizeof buf) == 4);
		CHECK(strcmp(buf, "abcd") == 0);
	}

	/* The terminal table: this system has none, so nothing is found. */
	{
		if (access("/etc/ttys", F_OK) != 0) {
			CHECK(setttyent() == 0);
			CHECK(getttyent() == NULL);
			CHECK(getttynam("tty1") == NULL);
		}
		CHECK(endttyent() == 1);
	}

	/* Obstacks: objects grow across chunks, printf appends, free unwinds. */
	{
		struct obstack h;
		memset(&h, 0, sizeof h);
		CHECK(_obstack_begin(&h, 64, 0, chunk_alloc, free) == 1);
		grow(&h, "first", 6);
		char *first = finish(&h);
		char big[300];
		memset(big, 'x', sizeof big);
		grow(&h, "ab", 2);
		grow(&h, big, sizeof big);
		char *second = finish(&h);
		CHECK(strcmp(first, "first") == 0);
		CHECK(second[0] == 'a' && second[1] == 'b' && second[301] == 'x');
		CHECK(_obstack_allocated_p(&h, first) && _obstack_allocated_p(&h, second));
		CHECK(_obstack_memory_used(&h) >= 64 + 302);
		CHECK(obstack_printf(&h, "%d-%s", 42, "ok") == 5);
		grow(&h, "", 1);
		char *printed = finish(&h);
		CHECK(strcmp(printed, "42-ok") == 0);
		obstack_free(&h, first);
		CHECK(h.next_free == first && !_obstack_allocated_p(&h, second));
		obstack_free(&h, NULL);
	}

	/* The new mount API reaches the kernel: refused without privilege, or
	 * a descriptor error for a descriptor that is not one. */
	{
		int fs = fsopen("tmpfs", 0);
		CHECK(fs >= 0 || kernel_refused(fs));
		if (fs >= 0)
			close(fs);
		int picked = fspick(AT_FDCWD, "/", 0);
		CHECK(picked >= 0 || kernel_refused(picked));
		if (picked >= 0)
			close(picked);
		/* The kernel checks privilege and arguments before the descriptor,
		   so any of those answers says the call arrived; EFAULT would say
		   an argument was passed wrongly. */
		errno = 0;
		int ret = fsconfig(-1, 0, "source", "none", 0);
		CHECK(ret == -1 && (errno == EBADF || errno == EINVAL || kernel_refused(ret)));
		errno = 0;
		ret = fsmount(-1, 0, 0);
		CHECK(ret == -1 && (errno == EBADF || errno == EINVAL || kernel_refused(ret)));
	}

	/* The lock kind is kept, and a lock made with it works. */
	{
		pthread_rwlockattr_t attr;
		int kind = -1;
		CHECK(pthread_rwlockattr_init(&attr) == 0);
		CHECK(pthread_rwlockattr_getkind_np(&attr, &kind) == 0 && kind == 0);
		CHECK(pthread_rwlockattr_setkind_np(&attr, 2) == 0);
		CHECK(pthread_rwlockattr_getkind_np(&attr, &kind) == 0 && kind == 2);
		CHECK(pthread_rwlockattr_setkind_np(&attr, 3) == EINVAL);
		pthread_rwlock_t lock;
		CHECK(pthread_rwlock_init(&lock, &attr) == 0);
		CHECK(pthread_rwlock_rdlock(&lock) == 0 && pthread_rwlock_unlock(&lock) == 0);
		CHECK(pthread_rwlock_wrlock(&lock) == 0 && pthread_rwlock_unlock(&lock) == 0);
		pthread_rwlock_destroy(&lock);
		pthread_rwlockattr_destroy(&attr);
	}

	return t_status;
}
