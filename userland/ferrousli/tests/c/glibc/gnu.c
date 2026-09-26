/*
 * GNU's library functions beyond POSIX that Chrome's libraries call:
 * strerrorname_np, parse_printf_format, the reentrant random_r family,
 * arc4random, mallinfo2, malloc_trim, free_sized and free_aligned_sized.
 * Where the answer is fixed, it is glibc 2.43's.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <malloc.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "check.h"

const char *strerrorname_np(int);
const char *strerrordesc_np(int);
size_t parse_printf_format(const char *, size_t, int *);
void arc4random_buf(void *, size_t);
uint32_t arc4random(void);
uint32_t arc4random_uniform(uint32_t);
void free_sized(void *, size_t);
void free_aligned_sized(void *, size_t, size_t);

#ifndef __GLIBC__
struct random_data {
	int32_t *fptr, *rptr, *state;
	int rand_type, rand_deg, rand_sep;
	int32_t *end_ptr;
};
int random_r(struct random_data *, int32_t *);
int srandom_r(unsigned, struct random_data *);
int initstate_r(unsigned, char *, size_t, struct random_data *);
int setstate_r(char *, struct random_data *);
struct mallinfo2 {
	size_t arena, ordblks, smblks, hblks, hblkhd, usmblks, fsmblks, uordblks, fordblks, keepcost;
};
struct mallinfo2 mallinfo2(void);
int malloc_trim(size_t);
#endif

enum { PA_INT, PA_CHAR, PA_WCHAR, PA_STRING, PA_WSTRING, PA_POINTER, PA_FLOAT, PA_DOUBLE };
#define PA_FLAG_LONG_LONG (1 << 8)
#define PA_FLAG_LONG (1 << 9)
#define PA_FLAG_SHORT (1 << 10)
#define PA_FLAG_PTR (1 << 11)

static int types_are(const char *fmt, size_t want_n, const int *want)
{
	int got[16];
	memset(got, 0xff, sizeof got);
	if (parse_printf_format(fmt, 16, got) != want_n)
		return 0;
	for (size_t i = 0; i < want_n; i++)
		if (got[i] != want[i])
			return 0;
	return 1;
}

int main(void)
{
	CHECK(strcmp(strerrorname_np(EINVAL), "EINVAL") == 0);
	CHECK(strcmp(strerrorname_np(EAGAIN), "EAGAIN") == 0);
	CHECK(strcmp(strerrorname_np(EOPNOTSUPP), "EOPNOTSUPP") == 0);
	CHECK(strerrorname_np(-1) == NULL && strerrorname_np(4096) == NULL);
	CHECK(strerrordesc_np(ENOENT) != NULL && strerrordesc_np(4096) == NULL);

	{
		static const int a[] = { PA_INT, PA_STRING, PA_DOUBLE };
		static const int b[] = { PA_INT, PA_INT, PA_DOUBLE | PA_FLAG_LONG_LONG };
		static const int c[] = { PA_STRING, PA_INT | PA_FLAG_LONG };
		/* glibc's answers: `ll` is `long`'s where the two are one size,
		 * and `%lc` and `%ls` are reported as `char` and `char *`. */
		static const int d[] = { PA_CHAR, PA_INT | PA_FLAG_SHORT,
			sizeof(long) == sizeof(long long) ? PA_INT | PA_FLAG_LONG : PA_INT | PA_FLAG_LONG_LONG,
			PA_POINTER, PA_INT | PA_FLAG_PTR, PA_CHAR, PA_STRING, PA_INT | PA_FLAG_LONG };
		static const int e[] = { PA_INT, PA_INT };
		CHECK(types_are("%d %s %f %%", 3, a));
		CHECK(types_are("%*.*Lf", 3, b));
		CHECK(types_are("%2$ld %1$s", 2, c));
		CHECK(types_are("%hhd %hd %lld %p %n %lc %ls %zu%m", 8, d));
		CHECK(types_are("%2$*1$d", 2, e));
		CHECK(types_are("%y %d", 1, e));
		CHECK(parse_printf_format("plain", 0, NULL) == 0);
		CHECK(parse_printf_format("%d%d%d", 1, (int[1]){ 0 }) == 3);
	}

	{
		char state[128];
		struct random_data data;
		int32_t first, again, x;
		memset(&data, 0, sizeof data);
		CHECK(initstate_r(42, state, sizeof state, &data) == 0);
		CHECK(random_r(&data, &first) == 0 && first >= 0);
		for (int i = 0; i < 100; i++)
			CHECK(random_r(&data, &x) == 0 && x >= 0);
		CHECK(srandom_r(42, &data) == 0);
		CHECK(random_r(&data, &again) == 0 && again == first);
		CHECK(setstate_r(state, &data) == 0);
		errno = 0;
		CHECK(initstate_r(1, state, 4, &data) == -1 && errno == EINVAL);
	}

	{
		unsigned char bytes[64] = { 0 };
		int zeros = 0;
		arc4random_buf(bytes, sizeof bytes);
		for (size_t i = 0; i < sizeof bytes; i++)
			zeros += bytes[i] == 0;
		CHECK(zeros < 16);
		CHECK(arc4random_uniform(10) < 10 && arc4random_uniform(1) == 0);
		(void)arc4random();
	}

	{
		struct mallinfo2 info = mallinfo2();
		CHECK(info.usmblks == 0);
		CHECK(malloc_trim(0) == 0 || malloc_trim(0) == 1);
		void *p = malloc(100), *q = aligned_alloc(64, 128);
		CHECK(p != NULL && q != NULL);
		free_sized(p, 100);
		free_aligned_sized(q, 64, 128);
		free_sized(NULL, 0);
	}

	return t_status;
}
