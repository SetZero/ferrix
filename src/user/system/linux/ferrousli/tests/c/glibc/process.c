/*
 * glibc's process-wide names: the program's name under its four names,
 * __libc_stack_end, __timezone beside timezone, thread_local destructors
 * through __cxa_thread_atexit_impl (on a thread's exit, and for the thread
 * that calls exit, before the atexit handlers), __register_atfork and the
 * CPU set allocator.
 */

#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "check.h"

extern char *program_invocation_name, *program_invocation_short_name;
extern char *__progname, *__progname_full;
extern void *__libc_stack_end;
extern long __timezone;
int __cxa_thread_atexit_impl(void (*)(void *), void *, void *);
int __register_atfork(void (*)(void), void (*)(void), void (*)(void), void *);
cpu_set_t *__sched_cpualloc(size_t);
void __sched_cpufree(cpu_set_t *);

static int thread_ran[3];
static int main_dtor_ran;
static int child_handler_ran;

static void mark(void *slot)
{
	*(int *)slot += 1;
}

/* The second destructor must run before the first. */
static void second(void *slot)
{
	CHECK(thread_ran[0] == 0);
	*(int *)slot += 1;
}

static void *thread(void *arg)
{
	(void)arg;
	CHECK(__cxa_thread_atexit_impl(mark, &thread_ran[0], (void *)thread_ran) == 0);
	CHECK(__cxa_thread_atexit_impl(second, &thread_ran[1], (void *)thread_ran) == 0);
	return NULL;
}

static void at_exit(void)
{
	/* glibc runs the exiting thread's destructors first. */
	if (main_dtor_ran == 1 && t_status == 0)
		write(1, "ordered\n", 8);
}

static void in_child(void)
{
	child_handler_ran = 1;
}

int main(int argc, char **argv)
{
	int local;
	pthread_t t;
	pid_t pid;
	int status;

	(void)argc;
	CHECK(program_invocation_name == argv[0] && __progname_full == argv[0]);
	const char *slash = strrchr(argv[0], '/');
	CHECK(program_invocation_short_name == (slash ? slash + 1 : argv[0]));
	CHECK(&__progname == &program_invocation_short_name);
	CHECK(&__progname_full == &program_invocation_name);

	/* The stack starts above this frame, near argv. */
	CHECK(__libc_stack_end != NULL);
	CHECK((char *)__libc_stack_end > (char *)&local && (char *)__libc_stack_end <= (char *)argv);

	CHECK(setenv("TZ", "EST5", 1) == 0);
	tzset();
	CHECK(timezone == 5 * 3600 && __timezone == 5 * 3600);

	CHECK(pthread_create(&t, NULL, thread, NULL) == 0 && pthread_join(t, NULL) == 0);
	CHECK(thread_ran[0] == 1 && thread_ran[1] == 1);

	CHECK(__register_atfork(NULL, NULL, in_child, NULL) == 0);
	pid = fork();
	if (pid == 0)
		_exit(child_handler_ran ? 0 : 1);
	CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);

	cpu_set_t *set = __sched_cpualloc(1024);
	CHECK(set != NULL);
	CPU_ZERO_S(CPU_ALLOC_SIZE(1024), set);
	CPU_SET_S(1000, CPU_ALLOC_SIZE(1024), set);
	CHECK(CPU_ISSET_S(1000, CPU_ALLOC_SIZE(1024), set));
	__sched_cpufree(set);

	CHECK(atexit(at_exit) == 0);
	CHECK(__cxa_thread_atexit_impl(mark, &main_dtor_ran, (void *)thread_ran) == 0);
	exit(t_status);
}
