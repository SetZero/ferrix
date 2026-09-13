/*
 * Streams on files: every mode letter, errors and errno, positions and
 * pushback, lines and blocks, reopening, removal, temporary files, buffers,
 * cookies, the unlocked functions and the locks.
 *
 * Standard output carries only failures. Each test runs in a fresh directory,
 * so fixed file names are safe.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "test.h"

#define CHECK(c) do { \
	if (!(c)) \
		t_error("%s failed (errno %d)\n", #c, errno); \
} while (0)

/* The whole content of the file `name`, into `buf`. */
static const char *slurp(const char *name, char *buf, size_t size)
{
	FILE *f = fopen(name, "r");
	size_t n = 0;
	if (f) {
		n = fread(buf, 1, size - 1, f);
		fclose(f);
	}
	buf[n] = 0;
	return buf;
}

static void modes(void)
{
	FILE *f;
	char line[32], all[64];

	errno = 0;
	CHECK(fopen("missing", "r") == NULL && errno == ENOENT);
	errno = 0;
	CHECK(fopen("x", "q") == NULL && errno == EINVAL);

	CHECK((f = fopen("a.txt", "w")) != NULL);
	CHECK(fputs("hello\n", f) >= 0);
	errno = 0;
	CHECK(fgetc(f) == EOF && ferror(f) && errno == EBADF);
	clearerr(f);
	CHECK(!ferror(f) && !feof(f));
	CHECK(fclose(f) == 0);

	errno = 0;
	CHECK(fopen("a.txt", "wx") == NULL && errno == EEXIST);

	CHECK((f = fopen("a.txt", "a")) != NULL);
	CHECK(fputs("world\n", f) >= 0);
	CHECK(ftell(f) == 12);
	CHECK(fclose(f) == 0);

	CHECK((f = fopen("a.txt", "rb")) != NULL);
	CHECK(fgets(line, sizeof line, f) && strcmp(line, "hello\n") == 0);
	CHECK(fgets(line, sizeof line, f) && strcmp(line, "world\n") == 0);
	CHECK(fgets(line, sizeof line, f) == NULL && feof(f));
	CHECK(fputc('x', f) == EOF && ferror(f));
	CHECK(fclose(f) == 0);

	CHECK((f = fopen("a.txt", "r+")) != NULL);
	CHECK(fputs("J", f) >= 0);
	CHECK(fseek(f, 0, SEEK_CUR) == 0);
	CHECK(fgetc(f) == 'e');
	CHECK(fclose(f) == 0);
	CHECK(strcmp(slurp("a.txt", all, sizeof all), "Jello\nworld\n") == 0);

	CHECK((f = fopen("a.txt", "a+")) != NULL);
	CHECK(fgetc(f) == 'J');
	CHECK(fseek(f, 0, SEEK_SET) == 0);
	CHECK(fputs("!", f) >= 0);
	CHECK(fseek(f, -1, SEEK_END) == 0);
	CHECK(fgetc(f) == '!');
	CHECK(fclose(f) == 0);

	CHECK((f = fopen("a.txt", "re")) != NULL);
	CHECK(fcntl(fileno(f), F_GETFD) & FD_CLOEXEC);
	CHECK(fclose(f) == 0);
	CHECK((f = fopen("a.txt", "r")) != NULL);
	CHECK(!(fcntl(fileno(f), F_GETFD) & FD_CLOEXEC));
	CHECK(fclose(f) == 0);

	CHECK((f = fopen("a.txt", "w+")) != NULL);
	CHECK(fseek(f, 0, SEEK_END) == 0 && ftell(f) == 0);
	CHECK(fclose(f) == 0);
}

static void positions(void)
{
	FILE *f;
	fpos_t pos;
	int fds[2];

	CHECK((f = fopen("pos", "w+")) != NULL);
	CHECK(fwrite("0123456789", 1, 10, f) == 10);
	CHECK(ftell(f) == 10);
	rewind(f);
	CHECK(fgetc(f) == '0');
	CHECK(ftello(f) == 1);
	CHECK(fgetpos(f, &pos) == 0);
	CHECK(fgetc(f) == '1');
	CHECK(fsetpos(f, &pos) == 0);
	CHECK(fgetc(f) == '1');
	CHECK(fseek(f, -2, SEEK_END) == 0);
	CHECK(fgetc(f) == '8');
	errno = 0;
	CHECK(fseek(f, 0, 42) == -1 && errno == EINVAL);
	errno = 0;
	CHECK(fseek(f, -100, SEEK_SET) == -1 && errno == EINVAL);

	CHECK(ungetc('Z', f) == 'Z');
	CHECK(ftell(f) == 8);
	CHECK(fgetc(f) == 'Z');
	CHECK(fgetc(f) == '9');
	CHECK(ungetc('Q', f) == 'Q');
	CHECK(fseek(f, 0, SEEK_SET) == 0);
	CHECK(fgetc(f) == '0');
	CHECK(ungetc(EOF, f) == EOF);

	CHECK(fseek(f, 0, SEEK_END) == 0);
	CHECK(fgetc(f) == EOF && feof(f));
	CHECK(ungetc('e', f) == 'e' && !feof(f));
	CHECK(fgetc(f) == 'e');
	CHECK(fgetc(f) == EOF);
	CHECK(fclose(f) == 0);

	CHECK(pipe(fds) == 0);
	CHECK((f = fdopen(fds[0], "r")) != NULL);
	errno = 0;
	CHECK(ftell(f) == -1 && errno == ESPIPE);
	CHECK(fclose(f) == 0);
	close(fds[1]);
}

static void lines(void)
{
	FILE *f;
	char *line = NULL;
	size_t cap = 0;
	char s[4];
	int i;

	CHECK((f = fopen("lines", "w+")) != NULL);
	CHECK(fputs("one\ntwo\n\nlast", f) >= 0);
	rewind(f);
	CHECK(getline(&line, &cap, f) == 4 && strcmp(line, "one\n") == 0 && cap >= 5);
	CHECK(getline(&line, &cap, f) == 4 && strcmp(line, "two\n") == 0);
	CHECK(getline(&line, &cap, f) == 1 && strcmp(line, "\n") == 0);
	CHECK(getline(&line, &cap, f) == 4 && strcmp(line, "last") == 0);
	CHECK(getline(&line, &cap, f) == -1 && feof(f));
	rewind(f);
	CHECK(getdelim(&line, &cap, 'w', f) == 6 && strcmp(line, "one\ntw") == 0);
	errno = 0;
	CHECK(getline(NULL, &cap, f) == -1 && errno == EINVAL);
	free(line);

	rewind(f);
	CHECK(fgets(s, 4, f) == s && strcmp(s, "one") == 0);
	CHECK(fgets(s, 4, f) == s && strcmp(s, "\n") == 0);
	CHECK(fgets(s, 1, f) == s && s[0] == 0);
	CHECK(fgets(s, 0, f) == NULL);
	CHECK(fclose(f) == 0);

	CHECK((f = fopen("long", "w+")) != NULL);
	for (i = 0; i < 10000; i++)
		CHECK(fputc('a' + i % 26, f) == 'a' + i % 26);
	CHECK(fputc('\n', f) == '\n');
	rewind(f);
	line = NULL;
	cap = 0;
	CHECK(getline(&line, &cap, f) == 10001);
	CHECK(line && line[9999] == 'a' + 9999 % 26 && line[10000] == '\n' && line[10001] == 0);
	free(line);
	CHECK(fclose(f) == 0);
}

static void blocks(void)
{
	FILE *f;
	char buf[64];

	CHECK((f = fopen("blocks", "w+")) != NULL);
	CHECK(fwrite("abcdefghijkl", 3, 4, f) == 4);
	CHECK(fwrite("x", 0, 5, f) == 0);
	CHECK(fwrite("x", 5, 0, f) == 0);
	rewind(f);
	CHECK(fread(buf, 5, 3, f) == 2 && feof(f) && !ferror(f));
	CHECK(memcmp(buf, "abcdefghijkl", 12) == 0);
	CHECK(fread(buf, 1, 1, f) == 0);
	CHECK(fclose(f) == 0);

	/* A write larger than the buffer, then reads larger and smaller than it. */
	static char big[20000], back[20000];
	int i;
	for (i = 0; i < (int)sizeof big; i++)
		big[i] = i * 7;
	CHECK((f = fopen("big", "w+")) != NULL);
	CHECK(fwrite("<", 1, 1, f) == 1);
	CHECK(fwrite(big, 1, sizeof big, f) == sizeof big);
	CHECK(fwrite(">", 1, 1, f) == 1);
	CHECK(ftell(f) == 20002);
	rewind(f);
	CHECK(fgetc(f) == '<');
	CHECK(fread(back, 1, 100, f) == 100);
	CHECK(fread(back + 100, 1, sizeof back - 100, f) == sizeof back - 100);
	CHECK(memcmp(big, back, sizeof big) == 0);
	CHECK(fgetc(f) == '>');
	CHECK(fclose(f) == 0);
}

static void reopening(void)
{
	FILE *f, *g;
	char all[64];

	CHECK((f = fopen("r1", "w")) != NULL);
	CHECK(fputs("first", f) >= 0);
	CHECK((g = freopen("r2", "w", f)) == f);
	CHECK(fputs("second", g) >= 0);
	CHECK(fclose(g) == 0);
	CHECK(strcmp(slurp("r1", all, sizeof all), "first") == 0);
	CHECK(strcmp(slurp("r2", all, sizeof all), "second") == 0);

	CHECK((f = fopen("r3", "w")) != NULL);
	CHECK(fputs("x", f) >= 0);
	CHECK(freopen(NULL, "a", f) == f);
	CHECK(fseek(f, 0, SEEK_SET) == 0);
	CHECK(fputs("y", f) >= 0);
	CHECK(fclose(f) == 0);
	CHECK(strcmp(slurp("r3", all, sizeof all), "xy") == 0);

	CHECK((f = fopen("r4", "w")) != NULL);
	errno = 0;
	CHECK(freopen("no/such/dir", "r", f) == NULL && errno == ENOENT);

	/* Standard error keeps descriptor 2, which now names the file. */
	CHECK(freopen("err", "w", stderr) == stderr);
	CHECK(fileno(stderr) == 2);
	CHECK(fprintf(stderr, "via stderr ") == 11);
	CHECK(write(2, "raw", 3) == 3);
	CHECK(fclose(stderr) == 0);
	CHECK(strcmp(slurp("err", all, sizeof all), "via stderr raw") == 0);
}

static void removal(void)
{
	FILE *f;
	char b[8] = {0};

	CHECK((f = fopen("gone", "w")) != NULL);
	CHECK(fclose(f) == 0);
	CHECK(remove("gone") == 0);
	errno = 0;
	CHECK(remove("gone") == -1 && errno == ENOENT);
	CHECK(mkdir("dir", 0700) == 0);
	CHECK(remove("dir") == 0);

	CHECK((f = tmpfile()) != NULL);
	CHECK(fputs("temp", f) >= 0);
	rewind(f);
	CHECK(fread(b, 1, sizeof b - 1, f) == 4 && strcmp(b, "temp") == 0 && feof(f));
	CHECK(fclose(f) == 0);
}

static void buffers(void)
{
	FILE *f;
	static char mybuf[32];
	char all[64];

	CHECK((f = fopen("buf", "w+")) != NULL);
	CHECK(setvbuf(f, mybuf, _IOFBF, sizeof mybuf) == 0);
	CHECK(fputs("abc", f) >= 0);
	CHECK(memcmp(mybuf, "abc", 3) == 0);
	CHECK(strcmp(slurp("buf", all, sizeof all), "") == 0);
	CHECK(fflush(NULL) == 0);
	CHECK(strcmp(slurp("buf", all, sizeof all), "abc") == 0);
	errno = 0;
	CHECK(setvbuf(f, NULL, 99, 0) == -1 && errno == EINVAL);
	CHECK(fclose(f) == 0);

	CHECK((f = fopen("buf", "r")) != NULL);
	setbuf(f, NULL);
	CHECK(fgetc(f) == 'a');
	setbuffer(f, mybuf, 8);
	CHECK(fgetc(f) == 'b');
	setlinebuf(f);
	CHECK(fgetc(f) == 'c');
	CHECK(fgetc(f) == EOF);
	CHECK(fclose(f) == 0);

	CHECK((f = fopen("line", "w")) != NULL);
	setlinebuf(f);
	CHECK(fputs("one\ntw", f) >= 0);
	CHECK(strcmp(slurp("line", all, sizeof all), "one\n") == 0);
	CHECK(fclose(f) == 0);
	CHECK(strcmp(slurp("line", all, sizeof all), "one\ntw") == 0);
}

struct cookie {
	char buf[64];
	size_t pos, len;
	int closed;
};

static ssize_t cookie_read(void *c, char *b, size_t n)
{
	struct cookie *k = c;
	size_t left = k->len - k->pos;
	if (n > left)
		n = left;
	memcpy(b, k->buf + k->pos, n);
	k->pos += n;
	return n;
}

static ssize_t cookie_write(void *c, const char *b, size_t n)
{
	struct cookie *k = c;
	if (n > sizeof k->buf - k->pos)
		n = sizeof k->buf - k->pos;
	memcpy(k->buf + k->pos, b, n);
	k->pos += n;
	if (k->pos > k->len)
		k->len = k->pos;
	return n;
}

static int cookie_seek(void *c, off_t *offset, int whence)
{
	struct cookie *k = c;
	off_t base = whence == SEEK_SET ? 0 : whence == SEEK_CUR ? (off_t)k->pos : (off_t)k->len;
	if (base + *offset < 0 || base + *offset > (off_t)sizeof k->buf)
		return -1;
	k->pos = base + *offset;
	*offset = k->pos;
	return 0;
}

static int cookie_close(void *c)
{
	((struct cookie *)c)->closed = 1;
	return 0;
}

static void cookies(void)
{
	struct cookie c = {{0}, 0, 0, 0};
	cookie_io_functions_t io = { cookie_read, cookie_write, cookie_seek, cookie_close };
	char s[16];
	FILE *f;

	CHECK((f = fopencookie(&c, "w+", io)) != NULL);
	CHECK(fprintf(f, "cookie %d", 42) == 9);
	CHECK(c.len == 0);
	CHECK(fflush(f) == 0 && c.len == 9 && memcmp(c.buf, "cookie 42", 9) == 0);
	CHECK(fseek(f, 0, SEEK_SET) == 0);
	CHECK(fgets(s, sizeof s, f) == s && strcmp(s, "cookie 42") == 0);
	CHECK(fileno(f) == -1);
	CHECK(fclose(f) == 0 && c.closed);

	io.seek = NULL;
	io.read = NULL;
	CHECK((f = fopencookie(&c, "r", io)) != NULL);
	errno = 0;
	CHECK(fseek(f, 0, SEEK_SET) == -1);
	CHECK(fgetc(f) == EOF && ferror(f));
	CHECK(fclose(f) == 0);

	errno = 0;
	CHECK(fopencookie(&c, "z", io) == NULL && errno == EINVAL);
}

static void unlocked_and_locks(void)
{
	FILE *f;
	char s[8];

	CHECK((f = fopen("u", "w+")) != NULL);
	flockfile(f);
	CHECK(ftrylockfile(f) == 0);
	CHECK(putc_unlocked('a', f) == 'a');
	CHECK(fputc_unlocked('b', f) == 'b');
	CHECK(fputs_unlocked("cd", f) >= 0);
	CHECK(fwrite_unlocked("e", 1, 1, f) == 1);
	CHECK(fflush_unlocked(f) == 0);
	funlockfile(f);
	funlockfile(f);
	rewind(f);
	CHECK(getc_unlocked(f) == 'a');
	CHECK(fgetc_unlocked(f) == 'b');
	CHECK(fgets_unlocked(s, sizeof s, f) == s && strcmp(s, "cde") == 0);
	CHECK(fread_unlocked(s, 1, 1, f) == 0 && feof_unlocked(f));
	clearerr_unlocked(f);
	CHECK(!feof_unlocked(f) && !ferror_unlocked(f));
	CHECK(fileno_unlocked(f) == fileno(f));
	CHECK(putw(0x01020304, f) == 0);
	CHECK(fseek(f, -4, SEEK_END) == 0 && getw(f) == 0x01020304);
	CHECK(fclose(f) == 0);
}

static void flush_all(void)
{
	FILE *f;
	char all[16];

	CHECK((f = fopen("n", "w")) != NULL);
	CHECK(fputs("pending", f) >= 0);
	CHECK(strcmp(slurp("n", all, sizeof all), "") == 0);
	CHECK(fflush(NULL) == 0);
	CHECK(strcmp(slurp("n", all, sizeof all), "pending") == 0);
	CHECK(fclose(f) == 0);
}

int main(void)
{
	modes();
	positions();
	lines();
	blocks();
	removal();
	buffers();
	cookies();
	unlocked_and_locks();
	flush_all();
	reopening();
	return t_status;
}
