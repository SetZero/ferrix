/*
 * The functions that move, fill and compare memory a block at a time --
 * memcpy, memmove, memset and memcmp -- at every alignment of the destination
 * and several of the source, and every length up to 300, which crosses the
 * 16- and 64-byte steps of AArch64's vector loops several times over; memmove
 * over every overlap of up to 40 bytes either way; and the searches and the
 * copies beside a guard page, where reading a byte past what the caller named,
 * or before it, faults.
 */

#define _GNU_SOURCE
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include "check.h"

#define MAX 300
#define SLACK 32

static unsigned char src_buf[1024], dst_buf[1024], want_buf[1024];

static void *(*volatile pmemcpy)(void *, const void *, size_t);
static void *(*volatile pmemmove)(void *, const void *, size_t);
static void *(*volatile pmemset)(void *, int, size_t);
static int (*volatile pmemcmp)(const void *, const void *, size_t);

static unsigned char *aligned(unsigned char *p)
{
	return (unsigned char *)(((uintptr_t)p + 63) & -64);
}

/* Every length to 150, which covers each step of the vector loops at every
 * remainder, then every 17th to MAX. */
static int next_length(int len)
{
	return len < 150 ? len + 1 : len + 17;
}

/* A byte for position i that is never 0 and never the fill pattern. */
static unsigned char pattern(int i)
{
	return (unsigned char)(1 + (i * 7) % 250);
}

/* memcpy or memmove of len bytes, src at salign, dst at dalign, into a
 * buffer whose every other byte must be left alone. */
static int copies(int which, int salign, int dalign, int len)
{
	unsigned char *src = aligned(src_buf) + SLACK;
	unsigned char *dst = aligned(dst_buf) + SLACK;
	unsigned char *want = aligned(want_buf) + SLACK;
	for (int i = -SLACK; i < MAX + SLACK; i++) {
		src[i] = pattern(i + 1000);
		dst[i] = want[i] = 0xee;
	}
	for (int i = 0; i < len; i++)
		src[salign + i] = want[dalign + i] = pattern(i);
	void *r = which ? pmemmove(dst + dalign, src + salign, len)
			: pmemcpy(dst + dalign, src + salign, len);
	if (r != dst + dalign)
		return 0;
	for (int i = -SLACK; i < MAX + SLACK; i++)
		if (dst[i] != want[i])
			return 0;
	return 1;
}

/* memmove of len bytes within one buffer, the destination shift bytes from
 * the source, either way. */
static int overlaps(int align, int shift, int len)
{
	unsigned char *buf = aligned(dst_buf) + SLACK + 48 + align;
	unsigned char *want = aligned(want_buf) + SLACK + 48 + align;
	for (int i = -48; i < MAX; i++)
		buf[i] = want[i] = pattern(i + 48);
	for (int i = 0; i < len; i++)
		want[shift + i] = pattern(i + 48);
	if (pmemmove(buf + shift, buf, len) != buf + shift)
		return 0;
	for (int i = -48; i < MAX; i++)
		if (buf[i] != want[i])
			return 0;
	return 1;
}

static int fills(int align, int len)
{
	unsigned char *dst = aligned(dst_buf) + SLACK;
	int c = 0x100 | (len * 3 + align);
	for (int i = -SLACK; i < MAX + SLACK; i++)
		dst[i] = 0xee;
	if (pmemset(dst + align, c, len) != dst + align)
		return 0;
	for (int i = -SLACK; i < MAX + SLACK; i++) {
		int inside = i >= align && i < align + len;
		if (dst[i] != (inside ? (unsigned char)c : 0xee))
			return 0;
	}
	return 1;
}

static int sign(int x)
{
	return (x > 0) - (x < 0);
}

/* memcmp of len bytes, the two sides at aalign and balign, equal and then
 * with one byte different at pos, both ways round. */
static int compares(int aalign, int balign, int len, int pos)
{
	unsigned char *a = aligned(src_buf) + aalign;
	unsigned char *b = aligned(dst_buf) + balign;
	for (int i = 0; i < len; i++)
		a[i] = b[i] = pattern(i);
	if (pmemcmp(a, b, len) != 0)
		return 0;
	if (pos < 0)
		return 1;
	/* Unsigned: 0x80 is above 0x7f, whatever char's sign. */
	a[pos] = 0x80;
	b[pos] = 0x7f;
	if (sign(pmemcmp(a, b, len)) != 1 || sign(pmemcmp(b, a, len)) != -1)
		return 0;
	/* A later difference must not outvote the first one. */
	if (pos + 1 < len) {
		a[pos + 1] = 0x00;
		b[pos + 1] = 0xff;
		if (sign(pmemcmp(a, b, len)) != 1)
			return 0;
	}
	return 1;
}

/* Strings and buffers that end at the last byte before a guard page, or begin
 * at the first byte after one. */
static void guarded(void)
{
	long page = sysconf(_SC_PAGESIZE);
	CHECK(page > 0);
	unsigned char *map = mmap(0, 3 * page, PROT_READ | PROT_WRITE,
				  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK(map != MAP_FAILED);
	if (map == MAP_FAILED)
		return;
	CHECK(mprotect(map, page, PROT_NONE) == 0);
	CHECK(mprotect(map + 2 * page, page, PROT_NONE) == 0);
	unsigned char *start = map + page;
	unsigned char *end = map + 2 * page;
	static unsigned char copy[128];

	for (int len = 0; len < 100; len++) {
		/* A string whose NUL is the page's last byte. */
		char *s = (char *)end - len - 1;
		memset(s, 'x', len);
		s[len] = 0;
		CHECK(strlen(s) == (size_t)len);
		CHECK(strchr(s, 'y') == 0);
		CHECK(strchr(s, 0) == s + len);
		CHECK(strchrnul(s, 'y') == s + len);
		CHECK(strnlen(s, 4096) == (size_t)len);
		CHECK(memchr(s, 0, len + 1) == s + len);
		CHECK(memchr(s, 'y', len) == 0);
		CHECK(pmemcmp(s, s, len + 1) == 0);
		CHECK(pmemcpy(copy, s, len + 1) == copy);
		CHECK(pmemcmp(copy, s, len + 1) == 0);
		CHECK(pmemmove(s, s + 1, len) == s);
		CHECK(pmemset(end - len, 'z', len) == end - len);

		/* The same at the first byte after the guard below. */
		char *t = (char *)start;
		memset(t, 'x', len);
		t[len] = 0;
		CHECK(strlen(t) == (size_t)len);
		CHECK(strchrnul(t, 'y') == t + len);
		CHECK(memchr(t, 0, len + 1) == t + len);
		CHECK(pmemcmp(t, s, 0) == 0);
		CHECK(pmemcpy(copy, t, len + 1) == copy);
		CHECK(pmemmove(t + 1, t, len) == t + 1);
		CHECK(pmemset(start, 'z', len) == start);
	}
	munmap(map, 3 * page);
}

int main(void)
{
	static const int saligns[] = { 0, 1, 7, 8, 15 };

	pmemcpy = memcpy;
	pmemmove = memmove;
	pmemset = memset;
	pmemcmp = memcmp;

	for (int which = 0; which < 2; which++)
		for (unsigned s = 0; s < sizeof saligns / sizeof *saligns; s++)
			for (int d = 0; d < 16; d++)
				for (int len = 0; len <= MAX; len = next_length(len))
					if (!copies(which, saligns[s], d, len)) {
						CHECK(copies(which, saligns[s], d, len));
						return t_status;
					}

	for (int align = 0; align < 4; align++)
		for (int shift = -40; shift <= 40; shift++)
			for (int len = 0; len <= 200; len++)
				if (!overlaps(align, shift, len)) {
					CHECK(overlaps(align, shift, len));
					return t_status;
				}

	for (int align = 0; align < 16; align++)
		for (int len = 0; len <= MAX; len = next_length(len))
			if (!fills(align, len)) {
				CHECK(fills(align, len));
				return t_status;
			}

	for (int a = 0; a < 16; a += 5)
		for (int b = 0; b < 16; b += 3)
			for (int len = 0; len <= MAX; len = next_length(len)) {
				if (!compares(a, b, len, -1)) {
					CHECK(compares(a, b, len, -1));
					return t_status;
				}
				for (int pos = 0; pos < len; pos += len > 70 ? 13 : 1)
					if (!compares(a, b, len, pos)) {
						CHECK(compares(a, b, len, pos));
						return t_status;
					}
				if (len > 0 && !compares(a, b, len, len - 1)) {
					CHECK(compares(a, b, len, len - 1));
					return t_status;
				}
			}

	guarded();
	return t_status;
}
