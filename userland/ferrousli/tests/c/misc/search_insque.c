/*
 * insque and remque, adapted from libc-test's src/functional/search_insque.c
 * (MIT).
 */

#define _XOPEN_SOURCE 700
#include <search.h>
#include <stdlib.h>
#include "check.h"

struct q {
	struct q *n;
	struct q *p;
	int i;
};

static struct q *new(int i)
{
	struct q *q = malloc(sizeof *q);
	q->i = i;
	return q;
}

int main(void)
{
	struct q *q = new(0);
	struct q *p;
	int i;

	insque(q, 0);
	CHECK(q->n == 0 && q->p == 0);
	for (i = 1; i < 10; i++) {
		insque(new(i), q);
		q = q->n;
	}
	p = q;
	while (q) {
		CHECK(q->i == --i);
		q = q->p;
	}
	remque(p->p);
	CHECK(p->p->i == p->i - 2);
	CHECK(p->p->n->i == p->i);

	/* Inserting in the middle links both neighbours. */
	struct q *mid = new(100);
	struct q *before = p->p;
	insque(mid, before);
	CHECK(before->n == mid && mid->p == before && mid->n == p && p->p == mid);
	remque(p);
	CHECK(mid->n == 0);
	return t_status;
}
