/*
 * The robust mutex list as glibc keeps it, which code that links its own
 * robust mutexes into it reads -- the Steam client's browser helper does:
 * every thread registered with the kernel from its start, the fork child
 * too, with glibc's futex offset; and the head's prev link in the word
 * before it, pointing at the head while the list is empty and at the last
 * mutex while one is held, as each mutex's prev is the word before its next.
 */

#define _GNU_SOURCE
#include <pthread.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include "check.h"

struct robust_head {
	void *next;
	long futex_offset;
	void *pending;
};

/* The calling thread's registered head, or null. */
static struct robust_head *registered(size_t *len)
{
	struct robust_head *head = 0;

	*len = 0;
	if (syscall(SYS_get_robust_list, 0, &head, len) != 0)
		return 0;
	return head;
}

/* What Steam's helper checks before it links a mutex in: a head of the
   kernel's size with glibc's offset, whose first entry's prev (the word
   below it; the head's own when the list is empty) is the head. */
static int looks_like_glibcs(void)
{
	size_t len;
	struct robust_head *head = registered(&len);
	void **first;

	if (!head || len != sizeof *head)
		return 0;
	if (head->futex_offset != (long)(sizeof(void *) == 8 ? -32 : -20))
		return 0;
	first = (void **)((uintptr_t)head->next & ~(uintptr_t)1);
	return first[-1] == (void *)head;
}

static void *thread_check(void *arg)
{
	(void)arg;
	return (void *)(intptr_t)looks_like_glibcs();
}

int main(void)
{
	pthread_mutexattr_t attr;
	pthread_mutex_t m;
	pthread_t t;
	void *result = 0;
	size_t len;
	struct robust_head *head;
	int status;
	pid_t pid;

	/* The first thread, before any mutex, a new thread, and a fork child. */
	CHECK(looks_like_glibcs());
	CHECK(pthread_create(&t, 0, thread_check, 0) == 0);
	CHECK(pthread_join(t, &result) == 0 && result == (void *)1);
	pid = fork();
	if (pid == 0)
		_exit(looks_like_glibcs() ? 0 : 1);
	CHECK(pid > 0 && waitpid(pid, &status, 0) == pid);
	CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);

	/* A held robust shared mutex is the list's first entry; its prev is the
	   head, and the head's prev is it. Unlocked, the head's is the head's
	   own again. */
	CHECK(pthread_mutexattr_init(&attr) == 0);
	CHECK(pthread_mutexattr_setrobust(&attr, PTHREAD_MUTEX_ROBUST) == 0);
	CHECK(pthread_mutexattr_setpshared(&attr, PTHREAD_PROCESS_SHARED) == 0);
	CHECK(pthread_mutex_init(&m, &attr) == 0);
	CHECK(pthread_mutex_lock(&m) == 0);
	head = registered(&len);
	CHECK(head != 0);
	CHECK(looks_like_glibcs());
	CHECK(((void **)head)[-1] != (void *)head);
	CHECK(pthread_mutex_unlock(&m) == 0);
	CHECK(((void **)head)[-1] == (void *)head && head->next == (void *)head);
	CHECK(pthread_mutex_destroy(&m) == 0);
	return t_status;
}
