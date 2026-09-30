/*
 * tsearch, tfind, tdelete, twalk and tdestroy, adapted from libc-test's
 * src/functional/search_tsearch.c (MIT), with a larger tree whose balance is
 * measured through twalk's depths.
 */

#define _GNU_SOURCE
#include <search.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

struct e {
	char *k;
	int v;
};

static int count;
static void *root;
static struct e tab[100];
static struct e *cur = tab;

static int cmp(const void *a, const void *b)
{
	return strcmp(((struct e *)a)->k, ((struct e *)b)->k);
}

static int wantc = 'a';
static void act(const void *node, VISIT v, int d)
{
	struct e *e = *(void **)node;

	if (v == preorder)
		CHECK(e->k[0] >= wantc);
	if (v == endorder)
		CHECK(e->k[0] <= wantc);
	if (v == postorder)
		CHECK(e->k[0] == wantc);
	if (v == leaf)
		CHECK(e->k[0] == wantc);
	if (v == postorder || v == leaf)
		wantc++;
}

static const void *parent;
static char *searchkey;
static void getparent(const void *node, VISIT v, int d)
{
	static const void *p;
	struct e *e = *(void **)node;

	if (v == preorder || v == leaf)
		if (strcmp(searchkey, e->k) == 0)
			parent = p;
	if (v == preorder || v == postorder)
		p = node;
}

static struct e *get(char *k)
{
	void **p = tfind(&(struct e){.k = k}, &root, cmp);
	if (!p)
		return 0;
	return *p;
}

static struct e *set(char *k, int v)
{
	void **p;
	cur->k = k;
	cur->v = v;
	if (!get(k))
		count++;
	p = tsearch(cur++, &root, cmp);
	CHECK(p && strcmp(((struct e *)*p)->k, k) == 0);
	if (!p) {
		count--;
		return 0;
	}
	return *p;
}

static void *del(char *k)
{
	void *p = tdelete(&(struct e){.k = k}, &root, cmp);
	if (p)
		count--;
	return p;
}

static int intcmp(const void *a, const void *b)
{
	int x = *(const int *)a, y = *(const int *)b;
	return (x > y) - (x < y);
}

static int maxdepth, nodes;
static void depth(const void *node, VISIT v, int d)
{
	if (d > maxdepth)
		maxdepth = d;
	if (v == leaf || v == postorder)
		nodes++;
}

static int freed;
static void freekey(void *k)
{
	freed++;
}

int main(void)
{
	struct e *e;
	void *p;

	set("f", 6);
	set("b", 2);
	set("c", 3);
	set("e", 5);
	set("h", 8);
	set("g", 7);
	set("a", 1);
	set("d", 4);

	e = get("a");
	CHECK(e && e->v == 1);
	CHECK(get("z") == 0);
	e = set("g", 9);
	CHECK(e && e->v == 7);
	e = set("g", 9);
	CHECK(e && e->v == 7);
	e = set("i", 9);
	CHECK(e && e->v == 9);
	CHECK(del("foobar") == 0);

	twalk(root, act);
	CHECK(wantc == 'j');
	searchkey = "h";
	twalk(root, getparent);
	CHECK(parent != 0);
	p = del("h");
	CHECK(p == parent);

	e = *(void **)root;
	CHECK(del(e->k) != 0);

	for (; count; count--) {
		e = *(void **)root;
		CHECK(tdelete(e, &root, cmp) != 0);
	}
	CHECK(root == 0);

	/* 4095 keys inserted in order stay within an AVL tree's height. */
	static int keys[4095];
	void *big = 0;
	for (int i = 0; i < 4095; i++) {
		keys[i] = i;
		CHECK(tsearch(&keys[i], &big, intcmp) != 0);
	}
	twalk(big, depth);
	CHECK(nodes == 4095);
	CHECK(maxdepth <= 16);
	for (int i = 0; i < 4095; i += 3)
		CHECK(tdelete(&keys[i], &big, intcmp) != 0);
	CHECK(tfind(&keys[3], &big, intcmp) == 0);
	CHECK(*(int *)*(void **)tfind(&keys[4], &big, intcmp) == 4);
	tdestroy(big, freekey);
	CHECK(freed == 4095 - 1365);

	/* A null root pointer finds nothing. */
	CHECK(tfind(&keys[0], 0, intcmp) == 0);
	CHECK(tsearch(&keys[0], 0, intcmp) == 0);
	return t_status;
}
