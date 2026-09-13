/*
 * The misuse the allocator catches, one per run, chosen by the first argument.
 * Each should stop the program before main returns; returning is a failure.
 *
 * The calls go through pointers the compiler cannot see through, so that it
 * neither warns about the misuse nor optimises it away.
 */

#define _GNU_SOURCE
#include <malloc.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

static void *(*volatile malloc_p)(size_t) = malloc;
static void *(*volatile calloc_p)(size_t, size_t) = calloc;
static void *(*volatile realloc_p)(void *, size_t) = realloc;
static void *(*volatile aligned_alloc_p)(size_t, size_t) = aligned_alloc;
static void (*volatile free_p)(void *) = free;

static volatile int sink;

/* Zeros, where a header would be, at an address a block could have. */
static _Alignas(16) char decoy[64];

int main(int argc, char **argv)
{
	if (argc < 2)
		return 2;
	const char *what = argv[1];

	if (strcmp(what, "double-free") == 0) {
		char *p = malloc_p(100);
		char *keep = malloc_p(100);
		free_p(p);
		free_p(p);
		free_p(keep);
	} else if (strcmp(what, "double-free-aligned") == 0) {
		char *p = aligned_alloc_p(4096, 100);
		free_p(p);
		free_p(p);
	} else if (strcmp(what, "realloc-freed") == 0) {
		char *p = malloc_p(10);
		free_p(p);
		sink = realloc_p(p, 20) != NULL;
	} else if (strcmp(what, "interior") == 0) {
		char *p = calloc_p(1, 1000);
		free_p(p + 64);
	} else if (strcmp(what, "misaligned") == 0) {
		char *p = malloc_p(100);
		free_p(p + 1);
	} else if (strcmp(what, "static") == 0) {
		free_p(decoy + 32);
	} else if (strcmp(what, "overflow") == 0) {
		char *a = malloc_p(24);
		char *b = malloc_p(24);
		/* The first two blocks of a class are carved side by side. */
		if (b - a != 48)
			return 4;
		/* Run over a's block, into b's header. */
		memset(a, 'x', 48);
		free_p(b);
	} else if (strcmp(what, "use-after-free-list") == 0) {
		char *p = malloc_p(40);
		free_p(p);
		/* Overwrite the free list's link with an address holding zeros. */
		uintptr_t fake = (uintptr_t)decoy;
		memcpy(p, &fake, sizeof fake);
		sink = malloc_p(40) != NULL;
		sink = malloc_p(40) != NULL;
	} else if (strcmp(what, "large-use-after-free") == 0) {
		/* free unmaps a large block, so touching it faults. */
		char *p = malloc_p(1 << 20);
		p[0] = 1;
		free_p(p);
		sink = p[0];
	}
	return 3;
}
