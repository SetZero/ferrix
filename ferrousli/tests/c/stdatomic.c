/* POSIX.1-2024's stdatomic.h types and generic operations. */

#define _POSIX_C_SOURCE 202405L
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>

#include "check.h"

#define THREADS 4
#define ROUNDS 10000

static atomic_int count = ATOMIC_VAR_INIT(0);
static atomic_flag lock = ATOMIC_FLAG_INIT;
static int guarded_count;

static void *worker(void *argument)
{
	int i;

	(void)argument;
	for (i = 0; i < ROUNDS; i++) {
		(void)atomic_fetch_add_explicit(&count, 1, memory_order_relaxed);
		while (atomic_flag_test_and_set_explicit(&lock,
		    memory_order_acquire))
			;
		guarded_count++;
		atomic_flag_clear_explicit(&lock, memory_order_release);
	}
	return 0;
}

int main(void)
{
	pthread_t threads[THREADS];
	atomic_uint bits;
	atomic_long number;
	struct pair {
		int first;
		int second;
	};
	_Atomic(struct pair) pair;
	struct pair value;
	long expected;
	int i;

	atomic_init(&number, 7);
	CHECK(atomic_load_explicit(&number, memory_order_relaxed) == 7);
	atomic_store_explicit(&number, 11, memory_order_release);
	CHECK(atomic_exchange_explicit(&number, 13, memory_order_acq_rel) == 11);
	CHECK(atomic_load(&number) == 13);

	expected = 12;
	CHECK(!atomic_compare_exchange_strong(&number, &expected, 17));
	CHECK(expected == 13 && atomic_load(&number) == 13);
	CHECK(atomic_compare_exchange_strong_explicit(&number, &expected, 17,
	    memory_order_acq_rel, memory_order_acquire));
	CHECK(atomic_load(&number) == 17);
	expected = 17;
	while (!atomic_compare_exchange_weak(&number, &expected, 19))
		expected = 17;
	CHECK(atomic_load(&number) == 19);

	atomic_init(&bits, UINT32_C(0x30));
	CHECK(atomic_fetch_add(&bits, 2) == UINT32_C(0x30));
	CHECK(atomic_fetch_sub_explicit(&bits, 1, memory_order_relaxed) ==
	    UINT32_C(0x32));
	CHECK(atomic_fetch_or(&bits, UINT32_C(0x0c)) == UINT32_C(0x31));
	CHECK(atomic_fetch_xor(&bits, UINT32_C(0x03)) == UINT32_C(0x3d));
	CHECK(atomic_fetch_and(&bits, UINT32_C(0x1f)) == UINT32_C(0x3e));
	CHECK(atomic_load(&bits) == UINT32_C(0x1e));

	value.first = 1;
	value.second = 2;
	atomic_init(&pair, value);
	value = atomic_load(&pair);
	CHECK(value.first == 1 && value.second == 2);
	value.first = 3;
	value.second = 4;
	value = atomic_exchange(&pair, value);
	CHECK(value.first == 1 && value.second == 2);

	CHECK(kill_dependency(UINT32_C(23)) == UINT32_C(23));
	CHECK(atomic_is_lock_free(&count));
	atomic_signal_fence(memory_order_seq_cst);
	atomic_thread_fence(memory_order_seq_cst);

	for (i = 0; i < THREADS; i++)
		CHECK(pthread_create(&threads[i], 0, worker, 0) == 0);
	for (i = 0; i < THREADS; i++)
		CHECK(pthread_join(threads[i], 0) == 0);
	CHECK(atomic_load_explicit(&count, memory_order_acquire) ==
	    THREADS * ROUNDS);
	CHECK(guarded_count == THREADS * ROUNDS);

	atomic_flag_clear(&lock);
	CHECK(!atomic_flag_test_and_set(&lock));
	CHECK(atomic_flag_test_and_set(&lock));
	return t_status;
}
