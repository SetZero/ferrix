/*
 * crypt and crypt_r: the traditional DES hash and BSDi's extended form beside
 * the $ hashes, and the settings they refuse.
 *
 * The DES hashes of settings in the alphabet are libxcrypt's, from perl's
 * crypt. The two with bytes above 0x7f are the self-test vectors of musl
 * 1.2.5's crypt_des.c, and the MD5 one is libc-test's crypt.c (both MIT).
 */

#include <crypt.h>
#include <string.h>

#include "check.h"

#define TEST_KEY "\x80\xff\x80\x01 \x7f\x81\x80\x80\x0d\x0a\xff\x7f \x81 test"

static int is(const char *got, const char *want)
{
	return got && strcmp(got, want) == 0;
}

int main(void)
{
	CHECK(is(crypt("password", "ab"), "abJnggxhB/yWI"));
	CHECK(is(crypt("", ".."), "..X8NBuQ4l6uQ"));
	CHECK(is(crypt("12345678extra", "Az"), "Azc.VARqxkTMU"));
	CHECK(is(crypt(TEST_KEY, "zZ"), "zZgIU02pSikzc"));
	CHECK(is(crypt(TEST_KEY, "\x80" "x"), "\x80" "x22/wK52ZKGA"));

	/* What a login does: hash under the stored entry and compare. */
	CHECK(is(crypt("password", "abJnggxhB/yWI"), "abJnggxhB/yWI"));
	CHECK(!is(crypt("passwore", "abJnggxhB/yWI"), "abJnggxhB/yWI"));

	CHECK(is(crypt("test", "_J9..CCCC"), "_J9..CCCCZBIc.TMGpK."));
	CHECK(is(crypt(TEST_KEY, "_0.../9Zz"), "_0.../9ZzX7iSJNd21sU"));
	CHECK(is(crypt("123456789", "_1234abcd"), "_1234abcddAJdBhgpNwU"));

	/* The $ hashes still come first. */
	CHECK(is(crypt("", "$1$salt$"), "$1$salt$UsdFqFVB.FsuinRDK5eE.."));
	CHECK(is(crypt("password", "$2a$04$abcdefghijklmnopqrstuv"), "*"));

	/* A $ setting naming no hash is a DES salt, as musl reads it. */
	const char *dollar = crypt("password", "$9$salt$");
	CHECK(dollar && strlen(dollar) == 13 && strncmp(dollar, "$9", 2) == 0);

	/* Refusals: "*", or "x" for a setting that is itself "*", so that a
	 * locked entry never matches its own hash. */
	CHECK(is(crypt("password", ""), "*"));
	CHECK(is(crypt("password", "a"), "*"));
	CHECK(is(crypt("password", "a:"), "*"));
	CHECK(is(crypt("password", "_...."), "*"));
	CHECK(is(crypt("password", "_....abcd"), "*"));
	CHECK(is(crypt("password", "*"), "x"));

	struct crypt_data data;
	memset(&data, 0, sizeof data);
	CHECK(is(crypt_r("password", "ab", &data), "abJnggxhB/yWI"));
	CHECK(is(crypt_r("test", "_J9..CCCC", &data), "_J9..CCCCZBIc.TMGpK."));
	CHECK(is(crypt_r("password", "*", &data), "x"));

	return t_status;
}
