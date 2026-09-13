/*
 * Detached threads free their own memory while the main thread keeps
 * running.
 *
 * The address space is limited to 128 MiB, and 2000 threads with 256 KiB
 * stacks, half detached by attribute and half by pthread_detach, are created
 * one after another. Without freeing they would need 500 MiB, and creation
 * would fail with EAGAIN. Each thread reports on a pipe before it ends, and
 * the main thread waits for the report before creating the next, so only a
 * few can be exiting at once.
 */

#include <errno.h>
#include <pthread.h>
#include <sys/resource.h>
#include <unistd.h>

#include "check.h"

enum { THREADS = 2000 };

static int reports[2];

static void *report(void *arg)
{
	unsigned char byte = (unsigned char)(long)arg;
	write(reports[1], &byte, 1);
	return 0;
}

int main(void)
{
	CHECK(pipe(reports) == 0);
	struct rlimit limit = { 128L << 20, 128L << 20 };
	CHECK(setrlimit(RLIMIT_AS, &limit) == 0);

	pthread_attr_t detached;
	CHECK(pthread_attr_init(&detached) == 0);
	CHECK(pthread_attr_setstacksize(&detached, 256 << 10) == 0);
	pthread_attr_t joinable = detached;
	CHECK(pthread_attr_setdetachstate(&detached, PTHREAD_CREATE_DETACHED) == 0);
	int state = -1;
	CHECK(pthread_attr_getdetachstate(&detached, &state) == 0);
	CHECK(state == PTHREAD_CREATE_DETACHED);

	for (long i = 0; i < THREADS && t_status == 0; i++) {
		pthread_t t;
		if (i % 2) {
			CHECK(pthread_create(&t, &detached, report, (void *)i) == 0);
		} else {
			CHECK(pthread_create(&t, &joinable, report, (void *)i) == 0);
			CHECK(pthread_detach(t) == 0);
		}
		unsigned char byte;
		CHECK(read(reports[0], &byte, 1) == 1);
		CHECK(byte == (unsigned char)i);
	}
	return t_status;
}
