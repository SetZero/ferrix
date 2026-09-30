/*
 * gettext with no catalogue, and iconv between the Unicode encodings and
 * the single-byte sets, as GLib, ATK and libmount use them. Every expected
 * answer is glibc 2.43's, read from the same calls on the host.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <iconv.h>
#include <libintl.h>
#include <locale.h>
#include <string.h>

#include "check.h"

/* Converts `inlen` bytes from `from` to `to`, and checks the result, the
 * errno, the input left and the output's bytes. */
static int converts(const char *to, const char *from, const char *in, size_t inlen,
	size_t want_r, int want_errno, size_t want_left, const char *want, size_t want_len)
{
	char out[64], *ip = (char *)in, *op = out;
	size_t il = inlen, ol = sizeof out;
	iconv_t cd = iconv_open(to, from);
	if (cd == (iconv_t)-1)
		return 0;
	errno = 0;
	size_t r = iconv(cd, &ip, &il, &op, &ol);
	int e = r == (size_t)-1 ? errno : 0;
	iconv_close(cd);
	return r == want_r && e == want_errno && il == want_left
		&& (size_t)(op - out) == want_len && memcmp(out, want, want_len) == 0
		&& ip == in + (inlen - il) && ol == sizeof out - want_len;
}

#define FAIL ((size_t)-1)

int main(void)
{
	CHECK(strcmp(gettext("hello"), "hello") == 0);
	CHECK(strcmp(dgettext("d", "hi"), "hi") == 0);
	CHECK(strcmp(dcgettext("d", "hi", LC_MESSAGES), "hi") == 0);
	CHECK(strcmp(ngettext("one", "many", 1), "one") == 0);
	CHECK(strcmp(ngettext("one", "many", 0), "many") == 0);
	CHECK(strcmp(dngettext("d", "one", "many", 2), "many") == 0);
	CHECK(strcmp(dcngettext("d", "a", "b", 0, LC_MESSAGES), "b") == 0);

	CHECK(strcmp(textdomain(NULL), "messages") == 0);
	CHECK(strcmp(textdomain("glib20"), "glib20") == 0);
	CHECK(strcmp(textdomain(NULL), "glib20") == 0);
	CHECK(strcmp(textdomain(""), "messages") == 0);
	CHECK(strcmp(bindtextdomain("glib20", NULL), "/usr/share/locale") == 0);
	CHECK(strcmp(bindtextdomain("glib20", "/opt/share/locale"), "/opt/share/locale") == 0);
	CHECK(strcmp(bindtextdomain("glib20", NULL), "/opt/share/locale") == 0);
	CHECK(bindtextdomain("", "/x") == NULL && bindtextdomain(NULL, "/x") == NULL);
	CHECK(bind_textdomain_codeset("glib20", NULL) == NULL);
	CHECK(strcmp(bind_textdomain_codeset("glib20", "UTF-8"), "UTF-8") == 0);
	CHECK(strcmp(bind_textdomain_codeset("glib20", NULL), "UTF-8") == 0);

	/* The Unicode encodings, with glibc's byte orders and marks. */
	CHECK(converts("UTF-16", "UTF-8", "A\xc3\xa9", 3, 0, 0, 0, "\xff\xfe" "A\0\xe9\0", 6));
	CHECK(converts("UTF-32", "UTF-8", "A", 1, 0, 0, 0, "\xff\xfe\0\0" "A\0\0\0", 8));
	CHECK(converts("UCS-4", "UTF-8", "A", 1, 0, 0, 0, "\0\0\0A", 4));
	CHECK(converts("UCS-2", "UTF-8", "A", 1, 0, 0, 0, "A\0", 2));
	CHECK(converts("WCHAR_T", "UTF-8", "A", 1, 0, 0, 0, "A\0\0\0", 4));
	CHECK(converts("UTF-16BE", "UTF-8", "\xf0\x9f\x98\x80", 4, 0, 0, 0, "\xd8\x3d\xde\x00", 4));
	CHECK(converts("UTF-8", "UTF-16", "\x00\x41", 2, 0, 0, 0, "\xe4\x84\x80", 3));
	CHECK(converts("UTF-8", "UTF-16", "\xff\xfe\x41\x00", 4, 0, 0, 0, "A", 1));
	CHECK(converts("UTF-8", "UTF-16", "\xfe\xff\x00\x41", 4, 0, 0, 0, "A", 1));
	CHECK(converts("UTF-8", "UCS-4", "\0\0\0A", 4, 0, 0, 0, "A", 1));
	CHECK(converts("UTF-8", "UTF-16LE", "\x3d\xd8\x00\xde", 4, 0, 0, 0, "\xf0\x9f\x98\x80", 4));

	/* The single-byte sets, and what they cannot hold. */
	CHECK(converts("UTF-8", "latin1", "\xe9", 1, 0, 0, 0, "\xc3\xa9", 2));
	CHECK(converts("UTF-8", "CP1252", "\x80", 1, 0, 0, 0, "\xe2\x82\xac", 3));
	CHECK(converts("CP1252", "UTF-8", "\xe2\x82\xac", 3, 0, 0, 0, "\x80", 1));
	CHECK(converts("UTF-8", "ANSI_X3.4-1968", "a", 1, 0, 0, 0, "a", 1));
	CHECK(converts("utf8", "US-ASCII", "\x80", 1, FAIL, EILSEQ, 1, "", 0));
	CHECK(converts("ISO-8859-1", "UTF-8", "\xe2\x82\xac", 3, FAIL, EILSEQ, 3, "", 0));
	CHECK(converts("ASCII//TRANSLIT", "UTF-8", "a\xc3\xa9", 3, 1, 0, 0, "a?", 2));
	CHECK(converts("ASCII//IGNORE", "UTF-8", "a\xc3\xa9" "b", 4, FAIL, EILSEQ, 0, "ab", 2));

	/* Incomplete and invalid input, and a full output buffer. */
	CHECK(converts("UTF-8", "UTF-8", "a\xc3", 2, FAIL, EINVAL, 1, "a", 1));
	CHECK(converts("UTF-8", "UTF-8", "a\xff" "b", 3, FAIL, EILSEQ, 2, "a", 1));
	CHECK(converts("UTF-8", "UTF-8", "\xc0\x80", 2, FAIL, EILSEQ, 2, "", 0));
	CHECK(converts("UTF-8", "UTF-8", "\xed\xa0\x80", 3, FAIL, EILSEQ, 3, "", 0));
	{
		iconv_t cd = iconv_open("UTF-16LE", "UTF-8");
		char out[3], *ip = "abc", *op = out;
		size_t il = 3, ol = sizeof out;
		CHECK(cd != (iconv_t)-1);
		errno = 0;
		CHECK(iconv(cd, &ip, &il, &op, &ol) == (size_t)-1 && errno == E2BIG);
		CHECK(il == 2 && ol == 1 && out[0] == 'a');
		/* A null input starts over. */
		CHECK(iconv(cd, NULL, NULL, NULL, NULL) == 0);
		iconv_close(cd);
	}

	errno = 0;
	CHECK(iconv_open("UTF-8", "no-such-set") == (iconv_t)-1 && errno == EINVAL);
	return t_status;
}
