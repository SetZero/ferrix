/*
 * Atomics on AArch64 go through the outline helpers compiler-builtins puts in
 * the library (__aarch64_ldadd8_acq_rel and the rest), which take a single
 * LSE instruction or a load-exclusive loop according to
 * __aarch64_have_lse_atomics. A constructor sets that from AT_HWCAP, and it
 * must run after the library has read the auxiliary vector, or every core
 * would be treated as having no LSE. So the flag must agree with AT_HWCAP;
 * run on a core with LSE and on one without, both paths are taken, and
 * four threads' increments and a contended mutex must add up either way.
 * Elsewhere only the counting is checked.
 */

#include <pthread.h>
#include <stdatomic.h>
#include <sys/auxv.h>
#include "check.h"

#if defined(__aarch64__)
extern unsigned char __aarch64_have_lse_atomics;
/* HWCAP_ATOMICS, from arch/arm64/include/uapi/asm/hwcap.h. */
#define T_HWCAP_ATOMICS (1UL << 8)
#endif

#define THREADS 4
#define ROUNDS 20000

static _Atomic unsigned long counter;
static unsigned long guarded;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;

static void *work(void *arg)
{
	(void)arg;
	for (int i = 0; i < ROUNDS; i++) {
		atomic_fetch_add(&counter, 1);
		pthread_mutex_lock(&lock);
		guarded++;
		pthread_mutex_unlock(&lock);
	}
	return 0;
}

int main(void)
{
#if defined(__aarch64__)
	int lse = (getauxval(AT_HWCAP) & T_HWCAP_ATOMICS) != 0;
	CHECK(__aarch64_have_lse_atomics == lse);
#endif
	pthread_t threads[THREADS];
	for (int i = 0; i < THREADS; i++)
		CHECK(pthread_create(&threads[i], 0, work, 0) == 0);
	for (int i = 0; i < THREADS; i++)
		CHECK(pthread_join(threads[i], 0) == 0);
	CHECK(atomic_load(&counter) == (unsigned long)THREADS * ROUNDS);
	CHECK(guarded == (unsigned long)THREADS * ROUNDS);
	return t_status;
}
