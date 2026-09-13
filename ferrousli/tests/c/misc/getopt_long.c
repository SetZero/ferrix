/*
 * getopt_long and getopt_long_only: exact matches and abbreviations,
 * ambiguity, = and separate arguments, optional arguments, flags, the index,
 * -W, and the error reports.
 *
 * The expected traces, and the expected standard error in tests/c_misc.rs,
 * were generated once by building this file with -DGENERATE against the host
 * glibc 2.43.
 */

#define _GNU_SOURCE
#include <getopt.h>
#include <stdlib.h>
#include <unistd.h>
#include "trace.h"

static int flag;
static int n_case;

static const struct option longopts[] = {
	{"verbose", no_argument, &flag, 7},
	{"output", required_argument, 0, 'o'},
	{"out", no_argument, 0, 'u'},
	{"color", optional_argument, 0, 'C'},
	{"colour", optional_argument, 0, 'C'},
	{"column", required_argument, 0, 'L'},
	{"quiet", no_argument, 0, 'q'},
	{0, 0, 0, 0},
};

static void run(int only, const char *opts, char **argv, const char *want)
{
	int argc = 0, c, index;
	char label[32] = "getopt_long case ";
	while (argv[argc])
		argc++;
	t_reset();
	optind = 0;
	for (;;) {
		index = -1;
		flag = 0;
		c = only ? getopt_long_only(argc, argv, opts, longopts, &index)
			: getopt_long(argc, argv, opts, longopts, &index);
		if (c == -1)
			break;
		t_put("[");
		if (c > 32 && c < 127)
			t_putc(c);
		else
			t_putint(c);
		if (optarg) {
			t_put("=");
			t_put(optarg);
		}
		t_put(" ");
		t_putint(optind);
		t_put(" i");
		t_putint(index);
		if (flag) {
			t_put(" f");
			t_putint(flag);
		}
		t_put(" o");
		t_putint(optopt);
		t_put("]");
	}
	t_put(" end ");
	t_putint(optind);
	t_put(" argv:");
	for (int i = 1; i < argc; i++) {
		t_put(" ");
		t_put(argv[i]);
	}
	label[17] = 'a' + n_case++;
	t_compare(label, want[0] ? want : "(none)");
}

static const char *want[] = {
#include "getopt_long.want"
	""
};

#define ARGV(...) ((char *[]){"prog", __VA_ARGS__, 0})

int main(void)
{
	int i = 0;
	/* Exact names, abbreviations, = and separate arguments, a flag. */
	run(0, "o:q", ARGV("file1", "--verbose", "--output=out", "--output", "o2",
		"--out", "--outp=x", "--color", "--color=always", "--colou", "x",
		"--verb", "--q"), want[i++]);
	/* Mistakes: ambiguous, unknown, an argument where none is allowed, a
	 * missing one. */
	run(0, "o:q", ARGV("--col", "--co=1", "--unknown=3", "--verbose=1",
		"--column"), want[i++]);
	/* -- ends the options; + stops at a non-option. */
	run(0, "q", ARGV("--quiet", "--", "--verbose"), want[i++]);
	run(0, "+q", ARGV("--quiet", "file", "--quiet"), want[i++]);
	/* A leading : is silent and reports a missing argument as :. */
	run(0, ":o:", ARGV("--output", "--bogus", "--col"), want[i++]);
	/* getopt_long_only: -name is a long option unless it is a lone short
	 * option, and falls back to short options when no long one matches. */
	run(1, "o:qv", ARGV("-verbose", "-o", "x", "-out=y", "-q", "-qq", "-col",
		"-z", "-v", "-colors"), want[i++]);
	/* W; turns -W name into --name. */
	run(0, "W;q", ARGV("-W", "verbose", "-Wquiet", "-Wbogus", "-W"), want[i++]);
	return t_status;
}
