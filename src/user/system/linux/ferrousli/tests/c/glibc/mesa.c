/*
 * The names Mesa's software renderer brings in that no program before it
 * did: LLVM's __gethostname_chk, __readlink_chk, __mbstowcs_chk and
 * logf128, libpciaccess's iopl, libz3's pthread_mutex_clocklock, and
 * libstdc++'s __wmemset_chk. logf128 goes through the SysV ABI's xmm0, as
 * LLVM calls it.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#ifdef __x86_64__
#include <sys/io.h>
#endif
#include <unistd.h>
#include <wchar.h>

#include "check.h"

int __gethostname_chk(char *, size_t, size_t);
ssize_t __readlink_chk(const char *, char *, size_t, size_t);
size_t __mbstowcs_chk(wchar_t *, const char *, size_t, size_t);
wchar_t *__wmemset_chk(wchar_t *, wchar_t, size_t, size_t);
#ifdef __x86_64__
__float128 logf128(__float128);

/* The bits of a __float128, high word first. */
static void halves(__float128 x, unsigned long long out[2])
{
	unsigned long long words[2];

	memcpy(words, &x, sizeof words);
	out[0] = words[1];
	out[1] = words[0];
}
#endif

int main(void)
{
	char host[256], plain[256], link[64];
	wchar_t wide[8];
#ifdef __x86_64__
	unsigned long long got[2];
#endif

	/* The checked calls do the plain ones' work when the object holds it. */
	CHECK(__gethostname_chk(host, sizeof host, sizeof host) == 0);
	CHECK(gethostname(plain, sizeof plain) == 0 && !strcmp(host, plain));
	CHECK(symlink("/ferrousli-target", "/tmp/ferrousli-mesa-link") == 0 || errno == EEXIST);
	CHECK(__readlink_chk("/tmp/ferrousli-mesa-link", link, sizeof link, sizeof link) == 17);
	CHECK(!memcmp(link, "/ferrousli-target", 17));
	unlink("/tmp/ferrousli-mesa-link");
	CHECK(__mbstowcs_chk(wide, "abc", 8, 8) == 3 && wide[0] == L'a' && wide[3] == 0);
	CHECK(__wmemset_chk(wide, L'z', 8, 8) == wide && wide[0] == L'z' && wide[7] == L'z');

	/* libz3's pthread_mutex_clocklock: a free mutex is taken, a held one
	   times out on the monotonic clock, and an unknown clock is EINVAL. */
	{
		pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
		struct timespec at;

		CHECK(clock_gettime(CLOCK_MONOTONIC, &at) == 0);
		at.tv_nsec += 50000000;
		if (at.tv_nsec >= 1000000000) {
			at.tv_sec++;
			at.tv_nsec -= 1000000000;
		}
		CHECK(pthread_mutex_clocklock(&m, CLOCK_MONOTONIC, &at) == 0);
		CHECK(pthread_mutex_clocklock(&m, CLOCK_MONOTONIC, &at) == ETIMEDOUT);
		CHECK(pthread_mutex_clocklock(&m, CLOCK_PROCESS_CPUTIME_ID, &at) == EINVAL);
		CHECK(pthread_mutex_unlock(&m) == 0);
	}

#ifdef __x86_64__
	/* logf128: ln 2 and ln 10, rounded to nearest, and -inf at zero. */
	halves(logf128(2), got);
	CHECK(got[0] == 0x3ffe62e42fefa39eull && got[1] == 0xf35793c7673007e6ull);
	halves(logf128(10), got);
	CHECK(got[0] == 0x400026bb1bbb5551ull && got[1] == 0x582dd4adac5705a6ull);
	halves(logf128(0), got);
	CHECK(got[0] == 0xffff000000000000ull && got[1] == 0);

	/* iopl refuses a level past 3 before it asks for the privilege. */
	errno = 0;
	CHECK(iopl(4) == -1 && errno == EINVAL);
#endif
	return t_status;
}
