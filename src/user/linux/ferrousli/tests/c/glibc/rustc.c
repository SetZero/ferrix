/*
 * What rustc, cargo, LLVM and GCC import from glibc that ferrousli lacked:
 * glibc's double-underscore names for the locale functions and
 * pthread_key_create, rawmemchr, isnan and isinf as functions, logl,
 * shm_open, futimes, get_nprocs, sbrk, __xmknod, backtrace, the ucontext
 * family, and a mutex made by glibc's static recursive initialiser, which
 * puts the kind where glibc's layout keeps it.
 *
 * Each name a compiler knows as a builtin is called through a volatile
 * pointer, so that it is the library's function that runs, at -O2 too.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <locale.h>
#include <math.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <unistd.h>
#if defined(__x86_64__)
#include <ucontext.h>
#endif

#include "check.h"

#undef isnan
#undef isinf
int isnan(double);
int isinf(double);
int __isnan(double);
int __isinf(double);
int isnanf(float);
int isinff(float);
void *rawmemchr(const void *, int);
void *__rawmemchr(const void *, int);
int get_nprocs(void);
int get_nprocs_conf(void);
void *sbrk(intptr_t);
int backtrace(void **, int);
void backtrace_symbols_fd(void *const *, int, int);
locale_t __newlocale(int, const char *, locale_t);
locale_t __uselocale(locale_t);
void __freelocale(locale_t);
double __strtod_l(const char *, char **, locale_t);
int __pthread_key_create(pthread_key_t *, void (*)(void *));
#if !defined(__arm__)
int __xmknod(int, const char *, mode_t, dev_t *);
#endif

#if defined(__x86_64__)
static ucontext_t main_context, callee_context;
static char callee_stack[64 * 1024];
static long got[8];

/* Eight arguments: six in registers, two on the new stack. */
static void callee(long a, long b, long c, long d, long e, long f, long g, long h)
{
	got[0] = a; got[1] = b; got[2] = c; got[3] = d;
	got[4] = e; got[5] = f; got[6] = g; got[7] = h;
}
#endif

int main(void)
{
	/* rawmemchr: found with no length. */
	{
		void *(*volatile raw)(const void *, int) = rawmemchr;
		void *(*volatile under)(const void *, int) = __rawmemchr;
		static const char text[] = "compiler";
		CHECK(raw(text, 'p') == text + 3);
		CHECK(under(text, 0) == text + 8);
	}

	/* isnan and isinf as functions, with isinf's sign. */
	{
		int (*volatile nan_d)(double) = isnan;
		int (*volatile inf_d)(double) = isinf;
		int (*volatile nan_u)(double) = __isnan;
		int (*volatile inf_u)(double) = __isinf;
		int (*volatile nan_f)(float) = isnanf;
		int (*volatile inf_f)(float) = isinff;
		CHECK(nan_d(NAN) && !nan_d(1.0) && nan_u(NAN) && !nan_u(INFINITY));
		CHECK(inf_d(INFINITY) == 1 && inf_d(-INFINITY) == -1 && inf_d(NAN) == 0);
		CHECK(inf_u(-INFINITY) == -1 && inf_u(2.0) == 0);
		CHECK(nan_f(NAN) && !nan_f(1.0f) && inf_f(-INFINITY) == -1);
	}

	/* logl, computed in double. */
	{
		long double (*volatile log_l)(long double) = logl;
		CHECK(log_l(1.0L) == 0.0L);
		CHECK(fabsl(log_l(2.718281828459045L) - 1.0L) < 1e-15L);
	}

	/* glibc's names for the locale functions and the key constructor. */
	{
		locale_t c = __newlocale(LC_ALL_MASK, "C", 0);
		CHECK(c != 0);
		CHECK(__strtod_l("1.5", 0, c) == 1.5);
		locale_t old = __uselocale(c);
		CHECK(__uselocale(old) == c);
		__freelocale(c);
		pthread_key_t key;
		CHECK(__pthread_key_create(&key, 0) == 0);
		CHECK(pthread_setspecific(key, &key) == 0 && pthread_getspecific(key) == &key);
	}

	/* get_nprocs and sbrk, which moves the break and gives it back. */
	{
		CHECK(get_nprocs() >= 1 && get_nprocs_conf() >= 1);
		char *before = sbrk(0);
		CHECK(before != (void *)-1);
		CHECK(sbrk(4096) == before);
		CHECK(sbrk(0) == before + 4096);
		before[4095] = 1;
		CHECK(sbrk(-4096) == before + 4096);
		CHECK(sbrk(0) == before);
	}

	/* shm_open and shm_unlink, in /dev/shm. */
	{
		char name[64] = "/ferrousli-rustc-test-";
		unsigned pid = (unsigned)getpid();
		char digits[16];
		int n = 0;
		do { digits[n++] = (char)('0' + pid % 10); pid /= 10; } while (pid);
		size_t at = strlen(name);
		while (n)
			name[at++] = digits[--n];
		name[at] = 0;
		int fd = shm_open(name, O_CREAT | O_EXCL | O_RDWR, 0600);
		CHECK(fd >= 0);
		CHECK(ftruncate(fd, 4096) == 0);
		CHECK(fcntl(fd, F_GETFD) & FD_CLOEXEC);
		close(fd);
		CHECK(shm_unlink(name) == 0);
		errno = 0;
		CHECK(shm_open(name, O_RDWR, 0) == -1 && errno == ENOENT);
		errno = 0;
		CHECK(shm_open("/a/b", O_RDWR, 0) == -1 && errno == EINVAL);
	}

	/* futimes: to microseconds, now for a null pair, EINVAL out of range. */
	{
		int fd = open("stamped", O_RDWR | O_CREAT, 0600);
		CHECK(fd >= 0);
		struct timeval times[2] = {{1000000, 250000}, {2000000, 500000}};
		CHECK(futimes(fd, times) == 0);
		struct stat st;
		CHECK(fstat(fd, &st) == 0);
		CHECK(st.st_mtim.tv_sec == 2000000 && st.st_mtim.tv_nsec == 500000000);
		CHECK(futimes(fd, 0) == 0);
		times[1].tv_usec = 1000000;
		errno = 0;
		CHECK(futimes(fd, times) == -1 && errno == EINVAL);
		close(fd);
	}

#if !defined(__arm__)
	/* __xmknod: glibc's versioned mknod, the device read through a pointer. */
	{
		dev_t none = 0;
		CHECK(__xmknod(1, "fifo", S_IFIFO | 0600, &none) == 0);
		struct stat st;
		CHECK(stat("fifo", &st) == 0 && S_ISFIFO(st.st_mode));
	}
#endif

	/* backtrace: none without a loader to find libgcc's unwinder with; and
	 * backtrace_symbols_fd's line for an address no object holds. */
	{
		void *frames[8];
		int count = backtrace(frames, 8);
		CHECK(count >= 0 && count <= 8);
		int pipe_fds[2];
		CHECK(pipe(pipe_fds) == 0);
		void *nowhere[1] = {(void *)0x10};
		backtrace_symbols_fd(nowhere, 1, pipe_fds[1]);
		char line[32] = {0};
		CHECK(read(pipe_fds[0], line, sizeof line - 1) == 7);
		CHECK(strcmp(line, "[0x10]\n") == 0);
		close(pipe_fds[0]);
		close(pipe_fds[1]);
	}

	/* A mutex as glibc's PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP writes it:
	 * zero but for the kind, 1, in glibc's __kind -- the fifth int on a
	 * 64-bit target, the fourth on a 32-bit one. It must be recursive. */
	{
		static int words[10] = {0};
		words[sizeof(long) == 8 ? 4 : 3] = 1;
		pthread_mutex_t *m = (pthread_mutex_t *)(void *)words;
		CHECK(pthread_mutex_lock(m) == 0);
		CHECK(pthread_mutex_trylock(m) == 0);
		CHECK(pthread_mutex_unlock(m) == 0);
		CHECK(pthread_mutex_unlock(m) == 0);
		CHECK(pthread_mutex_unlock(m) == EPERM);
	}

#if defined(__x86_64__)
	/* The ucontext family: a function started on its own stack with eight
	 * arguments, returning to uc_link; and getcontext resumed twice. */
	{
		CHECK(getcontext(&callee_context) == 0);
		callee_context.uc_stack.ss_sp = callee_stack;
		callee_context.uc_stack.ss_size = sizeof callee_stack;
		callee_context.uc_link = &main_context;
		makecontext(&callee_context, (void (*)(void))callee, 8, 1L, 2L, 3L, 4L, 5L, 6L, 7L, 8L);
		CHECK(swapcontext(&main_context, &callee_context) == 0);
		for (int i = 0; i < 8; i++)
			CHECK(got[i] == i + 1);

		static volatile int resumed;
		ucontext_t again;
		resumed = 0;
		CHECK(getcontext(&again) == 0);
		if (++resumed < 3)
			setcontext(&again);
		CHECK(resumed == 3);
	}
#endif
	return t_status;
}
