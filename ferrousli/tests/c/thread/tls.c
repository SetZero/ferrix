/*
 * Every thread has its own errno and its own copy of each thread-local
 * variable, initialised from the program's TLS image and zeroed past it,
 * aligned as declared.
 *
 * Adapted from libc-test's functional/tls_init.c, tls_local_exec.c and
 * tls_align.c (MIT), with the shared-object half of tls_align folded in.
 *
 * All workers are alive at once: each sets its errno and variables, then
 * waits on a pipe until the main thread has started them all, and checks
 * that nothing changed underneath it.
 */

#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#include "check.h"

enum { THREADS = 8 };

_Thread_local int initialised = 42;
_Thread_local char zeroed[100];
_Thread_local _Alignas(64) long aligned = 7;

/* From tls_local_exec.c. */
static __thread char d1 = 11;
static __thread char d64 __attribute__((aligned(64))) = 22;
static __thread char d4096 __attribute__((aligned(4096))) = 33;
static __thread char z1 = 0;
static __thread char z64 __attribute__((aligned(64))) = 0;
static __thread char z4096 __attribute__((aligned(4096))) = 0;
static __thread const char *s1 = "s1";

/* From tls_align.c and tls_align_dso.c. */
__thread char c1 = 1;
__thread char xchar = 2;
__thread char c2 = 3;
__thread short xshort = 4;
__thread char c3 = 5;
__thread int xint = 6;
__thread char c4 = 7;
__thread long long xllong = 8;

static int gate[2];

static int fresh(void)
{
	int ok = initialised == 42 && aligned == 7;
	for (int i = 0; i < 100; i++)
		ok &= zeroed[i] == 0;
	ok &= d1 == 11 && d64 == 22 && d4096 == 33;
	ok &= z1 == 0 && z64 == 0 && z4096 == 0;
	ok &= !strcmp(s1, "s1");
	ok &= c1 == 1 && xchar == 2 && c2 == 3 && xshort == 4;
	ok &= c3 == 5 && xint == 6 && c4 == 7 && xllong == 8;
	return ok;
}

static int aligned_right(void)
{
	return (uintptr_t)&aligned % 64 == 0
		&& (uintptr_t)&d64 % 64 == 0 && (uintptr_t)&z64 % 64 == 0
		&& (uintptr_t)&d4096 % 4096 == 0 && (uintptr_t)&z4096 % 4096 == 0
		&& (uintptr_t)&xshort % _Alignof(short) == 0
		&& (uintptr_t)&xint % _Alignof(int) == 0
		&& (uintptr_t)&xllong % _Alignof(long long) == 0;
}

static void scribble(long id)
{
	errno = (int)id;
	initialised = (int)id;
	aligned = id * 3;
	zeroed[99] = (char)id;
	d4096 = (char)id;
	z4096 = (char)(id + 1);
	xllong = id * 5;
}

static int kept(long id)
{
	return errno == id && initialised == id && aligned == id * 3
		&& zeroed[99] == (char)id && d4096 == (char)id
		&& z4096 == (char)(id + 1) && xllong == id * 5;
}

static void *worker(void *arg)
{
	long id = (long)arg;
	if (errno != 0 || !fresh() || !aligned_right())
		return (void *)1;
	scribble(id);
	char byte;
	if (read(gate[0], &byte, 1) != 1)
		return (void *)2;
	if (!kept(id))
		return (void *)3;
	return 0;
}

int main(void)
{
	CHECK(fresh());
	CHECK(aligned_right());
	CHECK(pipe(gate) == 0);
	scribble(1000);

	pthread_t t[THREADS];
	for (long i = 0; i < THREADS; i++)
		CHECK(pthread_create(&t[i], 0, worker, (void *)(i + 1)) == 0);
	for (int i = 0; i < THREADS; i++)
		CHECK(write(gate[1], "x", 1) == 1);
	for (int i = 0; i < THREADS; i++) {
		void *result = (void *)-1;
		CHECK(pthread_join(t[i], &result) == 0);
		CHECK(result == 0);
	}
	CHECK(kept(1000));

	/* From tls_init.c: rounds of threads, each seeing the image. */
	for (int round = 0; round < 2; round++) {
		for (long i = 0; i < 5; i++) {
			CHECK(pthread_create(&t[i], 0, worker, (void *)(i + 1)) == 0);
			CHECK(write(gate[1], "x", 1) == 1);
		}
		for (int i = 0; i < 5; i++) {
			void *result = (void *)-1;
			CHECK(pthread_join(t[i], &result) == 0);
			CHECK(result == 0);
		}
	}
	return t_status;
}
