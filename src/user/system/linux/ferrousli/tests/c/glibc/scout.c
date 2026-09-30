/*
 * The names the 64-bit half of the Steam Runtime's scout libraries import,
 * built against glibc 2.15, and bash's getcwd(NULL, 0): glibc's inline and
 * internal names, its checked pread, stpncpy and wctomb, __xmknodat, the
 * argz functions libltdl calls, canonicalize_file_name, getspnam and
 * endspent, ftime, mq_unlink, wordexp, pthread_attr_setaffinity_np, and
 * the cleanup frames glibc's pthread_cleanup_push macro registers. With
 * the argument "pread", a checked pread is given a buffer too small and
 * must abort before reading.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <pthread.h>
#include <sched.h>
#include <shadow.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/timeb.h>
#include <time.h>
#include <unistd.h>
#include <wchar.h>
#include <wordexp.h>

#include "check.h"

int _IO_getc(FILE *);
char *__strndup(const char *, size_t);
char *__strsep_g(char **, const char *);
double __strtod_internal(const char *, char **, int);
long long strtoq(const char *, char **, int);
unsigned long long strtouq(const char *, char **, int);
char *__secure_getenv(const char *);
ssize_t __pread_chk(int, void *, size_t, off_t, size_t);
char *__stpncpy_chk(char *, const char *, size_t, size_t);
int __wctomb_chk(char *, wchar_t, size_t);
int __xmknodat(int, int, const char *, mode_t, dev_t *);
int argz_create_sep(const char *, int, char **, size_t *);
int argz_insert(char **, size_t *, char *, const char *);
int argz_append(char **, size_t *, const char *, size_t);
void argz_stringify(char *, size_t, int);

/* glibc's __pthread_unwind_buf_t, and the calls its pthread_cleanup_push
   and pthread_cleanup_pop macros make, as they expand them. */
#if defined(__x86_64__)
typedef long jump_words[8];
#elif defined(__aarch64__)
typedef long jump_words[22];
#else
typedef int jump_words[64] __attribute__((aligned(8)));
#endif
typedef struct {
	struct {
		jump_words jump;
		int mask_was_saved;
	} cancel_jmp_buf[1];
	void *pad[4];
} unwind_buf;
int __sigsetjmp(void *, int) __attribute__((returns_twice));
void __pthread_register_cancel(unwind_buf *);
void __pthread_unregister_cancel(unwind_buf *);
void __pthread_unwind_next(unwind_buf *) __attribute__((noreturn));

static char order[8];

static void note(void *what)
{
	strcat(order, what);
}

/* A thread that exits inside a glibc frame, itself inside one of this
   library's own: the handlers run innermost first. */
static void *exits_in_frames(void *arg)
{
	pthread_cleanup_push(note, "o");
	unwind_buf buf;
	if (__sigsetjmp(buf.cancel_jmp_buf, 0)) {
		note("g");
		__pthread_unwind_next(&buf);
	}
	__pthread_register_cancel(&buf);
	if (arg)
		pthread_exit((void *)7);
	__pthread_unregister_cancel(&buf);
	pthread_cleanup_pop(0);
	return (void *)8;
}

static int bound_cpu = -1;

static void *reads_its_cpus(void *arg)
{
	cpu_set_t set;
	(void)arg;
	if (sched_getaffinity(0, sizeof set, &set) == 0 && CPU_COUNT(&set) == 1)
		bound_cpu = CPU_ISSET(0, &set) ? 0 : 1;
	return NULL;
}

int main(int argc, char **argv)
{
	char small[4], big[64], path[4096], *argz = NULL, *rest;
	size_t argz_len = 0;
	int fd = open("data", O_RDWR | O_CREAT, 0600);

	CHECK(fd >= 0 && write(fd, "hello world", 11) == 11);
	if (argc > 1 && !strcmp(argv[1], "pread")) {
		memset(small, 'z', sizeof small);
		__pread_chk(fd, small, 8, 0, sizeof small);
		/* Not reached: eight bytes do not fit in four. */
		return small[0] == 'z' ? 3 : 4;
	}

	/* bash's getcwd(NULL, 0), and one of a size that does not fit. */
	char *cwd = getcwd(NULL, 0);
	CHECK(cwd != NULL && cwd[0] == '/' && getcwd(path, sizeof path) && !strcmp(cwd, path));
	char *sized = getcwd(NULL, strlen(cwd) + 1);
	CHECK(sized != NULL && !strcmp(sized, cwd));
	free(sized);
	errno = 0;
	CHECK(getcwd(NULL, 1) == NULL && errno == ERANGE);
	free(cwd);

	/* The internal and older names are the standard functions. */
	CHECK(lseek(fd, 0, SEEK_SET) == 0);
	FILE *f = fdopen(dup(fd), "r");
	CHECK(f != NULL && _IO_getc(f) == 'h');
	fclose(f);
	char *copy = __strndup("abcdef", 3);
	CHECK(copy != NULL && !strcmp(copy, "abc"));
	free(copy);
	strcpy(big, "a:b");
	rest = big;
	CHECK(!strcmp(__strsep_g(&rest, ":"), "a") && !strcmp(rest, "b"));
	CHECK(__strtod_internal("2.5x", &rest, 0) == 2.5 && *rest == 'x');
	CHECK(strtoq("-9000000000", NULL, 10) == -9000000000LL);
	CHECK(strtouq("ffffffffffffffff", NULL, 16) == 0xffffffffffffffffULL);
	CHECK(__secure_getenv("FERROUSLI_NO_SUCH_VARIABLE") == NULL);

	/* The checked calls do the plain ones' work when the object holds it. */
	CHECK(__pread_chk(fd, big, 5, 6, sizeof big) == 5 && !memcmp(big, "world", 5));
	memset(big, 'z', sizeof big);
	CHECK(__stpncpy_chk(big, "ab", 4, sizeof big) == big + 2 && !memcmp(big, "ab\0\0z", 5));
	CHECK(__wctomb_chk(big, L'A', sizeof big) == 1 && big[0] == 'A');

	/* __xmknodat reads the device through a pointer. */
	dev_t dev = 0;
	struct stat st;
	CHECK(__xmknodat(0, AT_FDCWD, "fifo", S_IFIFO | 0600, &dev) == 0);
	CHECK(stat("fifo", &st) == 0 && S_ISFIFO(st.st_mode));

	/* libltdl's argz functions. */
	CHECK(argz_create_sep("a:b", ':', &argz, &argz_len) == 0 && argz_len == 4);
	CHECK(argz_insert(&argz, &argz_len, argz + 3, "x") == 0);
	CHECK(argz_append(&argz, &argz_len, "c", 2) == 0 && argz_len == 8);
	argz_stringify(argz, argz_len, ' ');
	CHECK(!strcmp(argz, "a x b c"));
	free(argz);

	/* canonicalize_file_name is realpath into memory from malloc. */
	CHECK(symlink("data", "link") == 0);
	char *canon = canonicalize_file_name("link");
	CHECK(canon != NULL && !strcmp(canon + strlen(canon) - 5, "/data"));
	free(canon);
	errno = 0;
	CHECK(canonicalize_file_name("missing") == NULL && errno == ENOENT);

	/* No such user in the shadow file, which a test cannot read anyway. */
	CHECK(getspnam("ferrousli-no-such-user") == NULL);
	endspent();

	/* ftime is the real-time clock to the millisecond. */
	struct timeb tb;
	time_t before = time(NULL);
	CHECK(ftime(&tb) == 0 && tb.time >= before && tb.time <= before + 1);
	CHECK(tb.millitm < 1000 && tb.timezone == 0 && tb.dstflag == 0);

	/* A queue that is not there, with or without its slash. */
	errno = 0;
	CHECK(mq_unlink("/ferrousli-no-such-queue") == -1 && (errno == ENOENT || errno == ENOSYS));

	/* wordexp asks the shell. */
	wordexp_t we;
	CHECK(wordexp("a 'b c' \"d\"", &we, 0) == 0 && we.we_wordc == 3);
	CHECK(!strcmp(we.we_wordv[0], "a") && !strcmp(we.we_wordv[1], "b c") && !strcmp(we.we_wordv[2], "d"));
	CHECK(we.we_wordv[3] == NULL);
	CHECK(wordexp("e", &we, WRDE_APPEND) == 0 && we.we_wordc == 4 && !strcmp(we.we_wordv[3], "e"));
	wordfree(&we);
	we.we_offs = 2;
	CHECK(wordexp("f g", &we, WRDE_DOOFFS) == 0 && we.we_wordc == 2);
	CHECK(we.we_wordv[0] == NULL && we.we_wordv[1] == NULL && !strcmp(we.we_wordv[3], "g"));
	wordfree(&we);
	CHECK(wordexp("$(echo x)", &we, WRDE_NOCMD) == WRDE_CMDSUB);
	CHECK(wordexp("a;b", &we, WRDE_NOCMD) == WRDE_BADCHAR);

	/* A thread created with one CPU in its attribute runs on that one. */
	cpu_set_t all, one, back;
	pthread_attr_t attr;
	pthread_t t;
	void *result;
	CHECK(sched_getaffinity(0, sizeof all, &all) == 0);
	int cpu = CPU_ISSET(0, &all) ? 0 : 1;
	CHECK(CPU_ISSET(cpu, &all));
	CPU_ZERO(&one);
	CPU_SET(cpu, &one);
	CHECK(pthread_attr_init(&attr) == 0);
	CHECK(pthread_attr_getaffinity_np(&attr, sizeof back, &back) == 0 && CPU_COUNT(&back) == CPU_SETSIZE);
	CHECK(pthread_attr_setaffinity_np(&attr, sizeof one, &one) == 0);
	CHECK(pthread_attr_getaffinity_np(&attr, sizeof back, &back) == 0 && CPU_EQUAL(&back, &one));
	CHECK(pthread_create(&t, &attr, reads_its_cpus, NULL) == 0 && pthread_join(t, NULL) == 0);
	CHECK(bound_cpu == cpu);
	CHECK(pthread_attr_destroy(&attr) == 0);

	/* glibc's cleanup frames: popped without running, or run by
	   pthread_exit innermost first, the thread's result kept. */
	CHECK(pthread_create(&t, NULL, exits_in_frames, NULL) == 0 && pthread_join(t, &result) == 0);
	CHECK(result == (void *)8 && order[0] == 0);
	CHECK(pthread_create(&t, NULL, exits_in_frames, "exit") == 0 && pthread_join(t, &result) == 0);
	CHECK(result == (void *)7 && !strcmp(order, "go"));

	close(fd);
	return t_status;
}
