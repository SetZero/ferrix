/*
 * getopt: permutation, the ordering modes, --, grouped options, required and
 * optional arguments, and the error reports. Each case records what every
 * call returned and how argv ended up.
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

static int n_case;

static void run(const char *opts, char **argv, const char *want)
{
	int argc = 0, c;
	char label[32] = "getopt case ";
	while (argv[argc])
		argc++;
	t_reset();
	optind = 0;
	while ((c = getopt(argc, argv, opts)) != -1) {
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
		t_put("]");
	}
	t_put(" end ");
	t_putint(optind);
	t_put(" argv:");
	for (int i = 1; i < argc; i++) {
		t_put(" ");
		t_put(argv[i]);
	}
	t_put(" optopt=");
	t_putint(optopt);
	label[12] = 'a' + n_case++;
	t_compare(label, want[0] ? want : "(none)");
}

static const char *want[] = {
#include "getopt.want"
	""
};

#define ARGV(...) ((char *[]){"prog", __VA_ARGS__, 0})

int main(void)
{
	int i = 0;
	/* Permuted: options come first, non-options after, -- stops. */
	run("ab:c::", ARGV("-a", "x", "-b", "val", "y", "-cfoo", "-c", "z", "--", "-a", "w"), want[i++]);
	/* + stops at the first non-option. */
	run("+ab:", ARGV("-a", "x", "-b", "1"), want[i++]);
	/* - returns non-options as option 1. */
	run("-ab:", ARGV("x", "-a", "y", "-bz", "w"), want[i++]);
	/* A leading : reports a missing argument as :, silently. */
	run(":ab:", ARGV("-z", "-b"), want[i++]);
	/* Without it, both errors are reported. */
	run("ab:", ARGV("-z", "-b"), want[i++]);
	/* Grouped options, one taking the rest of its word. */
	run("abc:", ARGV("-abcarg", "-ab", "-c", "v"), want[i++]);
	/* - alone is a non-option, and -- is consumed. */
	run("ab", ARGV("-", "-a", "--", "-b"), want[i++]);
	/* A permuted run ending in non-options. */
	run("a", ARGV("x", "y", "-a", "z", "-a"), want[i++]);
	/* An optional argument must be attached. */
	run("c::", ARGV("-c", "z", "-cz"), want[i++]);
	/* A required argument may look like an option. */
	run("ab:", ARGV("-b", "-a"), want[i++]);
	/* : is never an option. */
	run("ab:", ARGV("-:"), want[i++]);
	/* No arguments at all. */
	run("ab", ARGV(0), want[i++]);

	/* opterr = 0 silences the reports. */
	opterr = 0;
	run("ab", ARGV("-x", "-a"), want[i++]);
	opterr = 1;

	/* POSIXLY_CORRECT stops at the first non-option too. */
	setenv("POSIXLY_CORRECT", "1", 1);
	run("ab", ARGV("-a", "x", "-b"), want[i++]);
	unsetenv("POSIXLY_CORRECT");
	run("ab", ARGV("-a", "x", "-b"), want[i++]);

	/* optreset, from musl's header, restarts a scan. */
	{
		char **argv = ARGV("-a", "-b");
		optind = 0;
		CHECK(getopt(3, argv, "ab") == 'a');
#ifndef GENERATE
		optreset = 1;
		CHECK(getopt(3, argv, "ab") == 'a');
#endif
		CHECK(getopt(3, argv, "ab") == 'b');
		CHECK(getopt(3, argv, "ab") == -1);
	}
	return t_status;
}
