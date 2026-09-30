/*
 * The names the Steam client's 64-bit side and the libraries it loads
 * import that nothing before them did: streaming_client's __swprintf_chk
 * and glob at its version before glibc 2.27, libcef's protection keys,
 * FFmpeg's __finite and libm's _finite names, libnss_compat's fgetpos64,
 * fsetpos64 and innetgr, and Berkeley DB's pthread_yield. With the
 * argument "swprintf", a checked swprintf is given a limit larger than its
 * array and must abort before writing.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <glob.h>
#include <math.h>
#include <netdb.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wchar.h>

#include "check.h"

int __swprintf_chk(wchar_t *, size_t, int, size_t, const wchar_t *, ...);
int __fwprintf_chk(FILE *, int, const wchar_t *, ...);
int __finite(double);
int __finitef(float);
double __exp_finite(double);
float __powf_finite(float, float);
double __atan2_finite(double, double);
double __gamma_r_finite(double, int *);
int pthread_yield(void);
/* glob@GLIBC_2.2.5 in libc.so.6; a static link reaches it by this name. */
int __ferrousli_glob_before_2_27(const char *, int, int (*)(const char *, int), glob_t *);
#undef fgetpos64
#undef fsetpos64
int fgetpos64(FILE *, fpos_t *);
int fsetpos64(FILE *, const fpos_t *);

int main(int argc, char **argv)
{
	wchar_t w[8];
	char bytes[8];
	volatile double one = 1.0;
	volatile float two = 2.0f;
	int sign = 0, plain_sign = 0;
	fpos_t pos;
	glob_t g;

	if (argc > 1 && !strcmp(argv[1], "swprintf")) {
		wmemset(w, L'z', 8);
		__swprintf_chk(w, 16, 1, 8, L"%d", 1);
		/* Not reached: the limit is larger than the array. */
		return w[0] == L'z' ? 3 : 4;
	}

	/* A limit the array holds formats as swprintf, failing on overflow. */
	CHECK(__swprintf_chk(w, 8, 1, 8, L"%d-%ls", 42, L"ab") == 5 && !wcscmp(w, L"42-ab"));
	CHECK(__swprintf_chk(w, 3, 1, 8, L"%d", 12345) == -1);
	FILE *f = tmpfile();
	CHECK(f != NULL);
	CHECK(__fwprintf_chk(f, 1, L"x%dy", 7) == 3 && fflush(f) == 0);
	CHECK(pread(fileno(f), bytes, 8, 0) == 3 && !memcmp(bytes, "x7y", 3));
	fclose(f);

	/* The large-file names of fgetpos and fsetpos. */
	f = tmpfile();
	CHECK(f != NULL && fputs("hello", f) >= 0);
	CHECK(fgetpos64(f, &pos) == 0);
	CHECK(fputs("world", f) >= 0);
	CHECK(fsetpos64(f, &pos) == 0 && ftell(f) == 5 && fgetc(f) == 'w');
	fclose(f);

	/* The _finite names are the plain functions, and __finite is finite. */
	CHECK(__finite(1.0) == 1 && __finite(INFINITY) == 0 && __finite(NAN) == 0);
	CHECK(__finitef(-1.5f) == 1 && __finitef(-INFINITY) == 0 && finite(0.0) == 1);
	CHECK(__exp_finite(one) == exp(one) && __exp_finite(0.0) == 1.0);
	CHECK(__powf_finite(two, 10.0f) == 1024.0f);
	CHECK(__atan2_finite(one, one) == atan2(one, one));
	CHECK(__gamma_r_finite(-0.5 * one, &sign) == lgamma_r(-0.5 * one, &plain_sign));
	CHECK(sign == -1 && plain_sign == -1);

	/* pthread_yield is sched_yield. */
	CHECK(pthread_yield() == 0);

	/* No netgroup file, or no such group in it: not a member. */
	CHECK(innetgr("ferrousli-no-such-netgroup", "host", "user", "domain") == 0);
	CHECK(innetgr(NULL, NULL, NULL, NULL) == 0);

	/* glob finds a dangling link named literally; its version before 2.27
	   does not, but both find one read from its directory. */
	CHECK(symlink("nowhere", "dangling") == 0);
	CHECK(glob("dangling", 0, NULL, &g) == 0 && g.gl_pathc == 1 && !strcmp(g.gl_pathv[0], "dangling"));
	globfree(&g);
	CHECK(__ferrousli_glob_before_2_27("dangling", 0, NULL, &g) == GLOB_NOMATCH);
	CHECK(__ferrousli_glob_before_2_27("dangl*", 0, NULL, &g) == 0 && g.gl_pathc == 1);
	globfree(&g);
	CHECK(symlink("dangling", "second") == 0 && close(creat("file", 0600)) == 0);
	CHECK(__ferrousli_glob_before_2_27("file", GLOB_MARK, NULL, &g) == 0 && g.gl_pathc == 1);
	globfree(&g);
	CHECK(__ferrousli_glob_before_2_27("second", 0, NULL, &g) == GLOB_NOMATCH);

	/* Protection keys: where the processor and kernel have them, a key's
	   rights are read and written in PKRU; elsewhere pkey_alloc says so. */
	void *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	CHECK(page != MAP_FAILED);
	CHECK(pkey_mprotect(page, 4096, PROT_READ, -1) == 0);
	CHECK(pkey_mprotect(page, 4096, PROT_READ | PROT_WRITE, -1) == 0);
	int key = pkey_alloc(0, 0);
	if (key >= 0) {
		CHECK(pkey_mprotect(page, 4096, PROT_READ | PROT_WRITE, key) == 0);
		CHECK(pkey_get(key) == 0);
		CHECK(pkey_set(key, PKEY_DISABLE_WRITE) == 0 && pkey_get(key) == PKEY_DISABLE_WRITE);
		CHECK(pkey_set(key, 0) == 0 && pkey_get(key) == 0);
		*(volatile char *)page = 1;
		CHECK(pkey_free(key) == 0);
	} else {
		CHECK(errno == ENOSPC || errno == EINVAL || errno == ENOSYS);
	}
#ifdef __x86_64__
	errno = 0;
	CHECK(pkey_get(16) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(pkey_set(0, 4) == -1 && errno == EINVAL);
#else
	CHECK(pkey_get(0) == -1 && errno == ENOSYS);
#endif
	munmap(page, 4096);

	return t_status;
}
