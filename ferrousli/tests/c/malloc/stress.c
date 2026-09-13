/*
 * A long run of random malloc, calloc, realloc, aligned_alloc and free over a
 * table of slots. Each slot's bytes are a function of a per-slot seed, and are
 * checked before every change, so an overlap or a lost copy shows up.
 *
 * The generator is xorshift64*, written here so the run is the same everywhere.
 */

#define _GNU_SOURCE
#include <malloc.h>
#include <stdint.h>
#include <stdlib.h>
#include "check.h"

#define SLOTS 1024
#define OPS 200000

static uint64_t state = 0x9e3779b97f4a7c15u;

static uint64_t next(void)
{
	state ^= state >> 12;
	state ^= state << 25;
	state ^= state >> 27;
	return state * 0x2545f4914f6cdd1du;
}

struct slot {
	unsigned char *p;
	size_t n;
	unsigned char seed;
};

static struct slot slots[SLOTS];

/* Mostly small, sometimes medium, rarely large. */
static size_t random_size(void)
{
	uint64_t r = next();
	switch (r % 64) {
	case 0:
		return 131000 + (size_t)(r >> 8) % 400000;
	case 1: case 2: case 3:
		return (size_t)(r >> 8) % 20000;
	default:
		return (size_t)(r >> 8) % 300;
	}
}

static unsigned char byte(unsigned char seed, size_t at)
{
	return (unsigned char)(seed + at * 3);
}

static void fill(struct slot *s, size_t from)
{
	for (size_t at = from; at < s->n; at++)
		s->p[at] = byte(s->seed, at);
}

static void verify(const struct slot *s, size_t n)
{
	size_t bad = 0;
	for (size_t at = 0; at < n; at++)
		bad += s->p[at] != byte(s->seed, at);
	CHECK(bad == 0);
}

int main(void)
{
	for (int op = 0; op < OPS && t_status == 0; op++) {
		struct slot *s = &slots[next() % SLOTS];
		uint64_t what = next() % 10;
		if (s->p)
			verify(s, s->n);

		if (!s->p) {
			s->n = random_size();
			s->seed = (unsigned char)next();
			if (what == 0) {
				s->p = calloc(1, s->n);
				CHECK(s->p != NULL);
				for (size_t at = 0; at < s->n; at++)
					CHECK(s->p[at] == 0);
			} else if (what == 1) {
				size_t align = (size_t)1 << (next() % 17);
				s->p = aligned_alloc(align, s->n);
				CHECK(s->p != NULL);
				CHECK(((uintptr_t)s->p & (align - 1)) == 0);
			} else {
				s->p = malloc(s->n);
				CHECK(s->p != NULL);
			}
			CHECK(malloc_usable_size(s->p) >= s->n);
			fill(s, 0);
		} else if (what < 4) {
			size_t n = random_size();
			unsigned char *q = realloc(s->p, n);
			CHECK(q != NULL);
			s->p = q;
			size_t old = s->n;
			s->n = n;
			verify(s, old < n ? old : n);
			fill(s, old < n ? old : n);
		} else {
			free(s->p);
			s->p = NULL;
		}
	}
	for (int i = 0; i < SLOTS; i++) {
		if (slots[i].p)
			verify(&slots[i], slots[i].n);
		free(slots[i].p);
	}
	return t_status;
}
