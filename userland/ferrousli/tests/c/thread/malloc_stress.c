/*
 * 64 threads allocate, fill, verify, reallocate and free at once. Every
 * block is filled with a pattern made of its owner, its size and its offset,
 * and checked before it is resized or freed, so a block handed to two
 * threads, or corrupted by a racing allocator, is caught. Each thread leaves
 * one block behind for the main thread to verify and free, which frees
 * memory across threads.
 */

#include <pthread.h>
#include <stdlib.h>
#include <string.h>

#include "check.h"

enum { THREADS = 64, ROUNDS = 1500, SLOTS = 32 };

struct block {
	unsigned char *p;
	size_t size;
};

static unsigned char pattern(long owner, size_t size, size_t at)
{
	return (unsigned char)(owner * 31 + size * 7 + at);
}

static void fill(struct block *b, long owner)
{
	for (size_t i = 0; i < b->size; i++)
		b->p[i] = pattern(owner, b->size, i);
}

static int intact(const struct block *b, long owner, size_t upto)
{
	for (size_t i = 0; i < upto; i++)
		if (b->p[i] != pattern(owner, b->size, i))
			return 0;
	return 1;
}

static unsigned next(unsigned *seed)
{
	*seed = *seed * 1103515245 + 12345;
	return *seed >> 8;
}

static size_t pick_size(unsigned *seed)
{
	unsigned r = next(seed);
	if (r % 200 == 0)
		return 150000 + r % 50000;
	return r % 3000;
}

struct result {
	long failures;
	struct block left;
};

static void *work(void *arg)
{
	long id = (long)arg;
	unsigned seed = (unsigned)id * 7919 + 1;
	struct block slot[SLOTS] = { 0 };
	struct result *out = malloc(sizeof *out);
	if (!out)
		return 0;
	out->failures = 0;

	for (int round = 0; round < ROUNDS; round++) {
		struct block *b = &slot[next(&seed) % SLOTS];
		if (b->p && !intact(b, id, b->size))
			out->failures++;
		switch (next(&seed) % 3) {
		case 0:
			free(b->p);
			b->size = pick_size(&seed);
			b->p = malloc(b->size);
			break;
		case 1:
			free(b->p);
			b->size = pick_size(&seed);
			b->p = calloc(1, b->size);
			for (size_t i = 0; b->p && i < b->size; i++)
				if (b->p[i])
					out->failures++;
			break;
		default: {
			size_t old = b->size;
			size_t size = pick_size(&seed);
			unsigned char *p = realloc(b->p, size);
			if (!p && size) {
				out->failures++;
				continue;
			}
			/* What survives the move keeps the old pattern. */
			struct block moved = { p, old };
			if (p && !intact(&moved, id, old < size ? old : size))
				out->failures++;
			b->p = p;
			b->size = size;
			break;
		}
		}
		if (b->size && !b->p)
			out->failures++;
		else if (b->p)
			fill(b, id);
	}
	for (int i = 1; i < SLOTS; i++) {
		if (slot[i].p && !intact(&slot[i], id, slot[i].size))
			out->failures++;
		free(slot[i].p);
	}
	out->left = slot[0];
	return out;
}

int main(void)
{
	pthread_t t[THREADS];
	for (long i = 0; i < THREADS; i++)
		CHECK(pthread_create(&t[i], 0, work, (void *)i) == 0);
	for (long i = 0; i < THREADS; i++) {
		struct result *r = 0;
		CHECK(pthread_join(t[i], (void **)&r) == 0);
		CHECK(r != 0);
		if (!r)
			continue;
		CHECK(r->failures == 0);
		CHECK(!r->left.p || intact(&r->left, i, r->left.size));
		free(r->left.p);
		free(r);
	}
	return t_status;
}
