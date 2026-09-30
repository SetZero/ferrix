/*
 * How streams buffer, chosen by the argument. write() shows where buffered
 * output is at each moment: bytes written through it land at once, while
 * buffered output lands when the stream flushes.
 *
 *   full    standard output on a pipe is fully buffered
 *   line    a line-buffered stream flushes at each newline
 *   none    an unbuffered stream writes every call
 *   stderr  standard error is unbuffered
 *   exit    exit flushes buffered output
 *   _exit   _exit does not
 *   atexit  exit flushes after the handlers, so their output appears
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void handler(void)
{
	printf("handler\n");
}

int main(int argc, char **argv)
{
	const char *mode = argc > 1 ? argv[1] : "";

	if (strcmp(mode, "full") == 0) {
		printf("buffered\n");
		write(1, "raw\n", 4);
		return 0;
	}
	if (strcmp(mode, "line") == 0) {
		setvbuf(stdout, NULL, _IOLBF, 0);
		printf("line\npartial");
		write(1, "|raw|", 5);
		printf("\n");
		return 0;
	}
	if (strcmp(mode, "none") == 0) {
		setvbuf(stdout, NULL, _IONBF, 0);
		printf("a");
		write(1, "b", 1);
		putchar('c');
		write(1, "d", 1);
		fputs("e\n", stdout);
		return 0;
	}
	if (strcmp(mode, "stderr") == 0) {
		fprintf(stdout, "out ");
		fprintf(stderr, "err\n");
		write(2, "raw\n", 4);
		return 0;
	}
	if (strcmp(mode, "exit") == 0) {
		fwrite("x", 1, 1, stdout);
		exit(3);
	}
	if (strcmp(mode, "_exit") == 0) {
		fwrite("x", 1, 1, stdout);
		_exit(4);
	}
	if (strcmp(mode, "atexit") == 0) {
		atexit(handler);
		printf("main\n");
		return 0;
	}
	return 99;
}
