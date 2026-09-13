/*
 * Freed memory is reused: interleaved frees keep the survivors intact, a freed
 * block serves the next request of its class, and a million allocate-and-free
 * cycles run in bounded memory.
 */

#define _GNU_SOURCE
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

#define SLOTS 256

static char *slots[SLOTS];

int main(void)
{
	/* A freed small block is the next one its class hands out. */
	for (size_t n = 1; n <= 131056; n = n * 3 + 1) {
		char *p = malloc(n);
		CHECK(p != NULL);
		free(p);
		char *q = malloc(n);
		CHECK(q == p);
		free(q);
	}

	/* Interleaved: allocate all, free every third, reallocate, repeat. */
	for (int i = 0; i < SLOTS; i++) {
		slots[i] = malloc((size_t)i * 5 + 1);
		memset(slots[i], i, (size_t)i * 5 + 1);
	}
	for (int round = 0; round < 50; round++) {
		for (int i = round % 3; i < SLOTS; i += 3) {
			free(slots[i]);
			slots[i] = NULL;
		}
		for (int i = 0; i < SLOTS; i++) {
			if (slots[i]) {
				size_t bad = 0;
				for (int at = 0; at < i * 5 + 1; at++)
					bad += slots[i][at] != (char)i;
				CHECK(bad == 0);
			} else {
				slots[i] = malloc((size_t)i * 5 + 1);
				CHECK(slots[i] != NULL);
				memset(slots[i], i, (size_t)i * 5 + 1);
			}
		}
	}
	for (int i = 0; i < SLOTS; i++)
		free(slots[i]);

	/*
	 * Without reuse, this would need about 48 GiB of small blocks and as
	 * many mappings of large ones as the kernel allows.
	 */
	for (int i = 0; i < 1000000; i++) {
		char *p = malloc(48000);
		CHECK(p != NULL);
		p[0] = 1;
		free(p);
	}
	for (int i = 0; i < 100000; i++) {
		char *p = malloc(1 << 20);
		CHECK(p != NULL);
		p[0] = 1;
		p[(1 << 20) - 1] = 1;
		free(p);
	}
	return t_status;
}
