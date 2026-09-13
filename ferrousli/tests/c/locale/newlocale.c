/*
 * newlocale, duplocale, freelocale and uselocale: the static locales musl
 * returns without allocating, objects changed in place, a thread's own
 * locale over the global one, and LC_GLOBAL_LOCALE.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <langinfo.h>
#include <locale.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include "check.h"

static int is(const char *got, const char *want)
{
	return got && !strcmp(got, want);
}

int main(void)
{
	/* The built-in locales come back as the same objects every time. */
	locale_t c = newlocale(LC_ALL_MASK, "C", 0);
	CHECK(c != 0);
	CHECK(newlocale(LC_ALL_MASK, "POSIX", 0) == c);
	locale_t utf8 = newlocale(LC_ALL_MASK, "C.UTF-8", 0);
	CHECK(utf8 != 0 && utf8 != c);
	CHECK(newlocale(LC_CTYPE_MASK, "C.UTF-8", c) == utf8);
	/* The categories not asked for come from the environment: empty, so C.UTF-8. */
	CHECK(newlocale(LC_CTYPE_MASK, "C.UTF-8", 0) == utf8);
	CHECK(newlocale(LC_ALL_MASK, "", 0) == utf8);
	CHECK(newlocale(0, "anything", 0) == utf8);
	/* Freeing a static locale does nothing. */
	freelocale(c);
	freelocale(utf8);
	CHECK(is(nl_langinfo_l(CODESET, c), "ASCII"));
	CHECK(is(nl_langinfo_l(CODESET, utf8), "UTF-8"));
	CHECK(is(nl_langinfo_l(NL_LOCALE_NAME(LC_CTYPE), utf8), "C.UTF-8"));
	CHECK(is(nl_langinfo_l(DAY_2, utf8), "Monday"));

	/* A null name fails. */
	errno = 0;
	CHECK(newlocale(LC_ALL_MASK, NULL, 0) == 0);
	CHECK(errno == EINVAL);

	/* Any other name gets an object of its own, changed in place as a base. */
	locale_t us = newlocale(LC_ALL_MASK, "en_US.UTF-8", 0);
	CHECK(us != 0 && us != c && us != utf8);
	CHECK(is(nl_langinfo_l(CODESET, us), "UTF-8"));
	CHECK(is(nl_langinfo_l(NL_LOCALE_NAME(LC_MESSAGES), us), "en_US.UTF-8"));
	CHECK(newlocale(LC_MESSAGES_MASK, "C", us) == us);
	CHECK(is(nl_langinfo_l(NL_LOCALE_NAME(LC_MESSAGES), us), "C"));
	CHECK(is(nl_langinfo_l(NL_LOCALE_NAME(LC_CTYPE), us), "en_US.UTF-8"));

	locale_t copy = duplocale(us);
	CHECK(copy != 0 && copy != us);
	CHECK(is(nl_langinfo_l(NL_LOCALE_NAME(LC_CTYPE), copy), "en_US.UTF-8"));
	CHECK(is(nl_langinfo_l(NL_LOCALE_NAME(LC_MESSAGES), copy), "C"));
	freelocale(us);
	CHECK(is(nl_langinfo_l(NL_LOCALE_NAME(LC_CTYPE), copy), "en_US.UTF-8"));
	freelocale(copy);

	locale_t c_copy = duplocale(c);
	CHECK(c_copy != 0 && c_copy != c);
	CHECK(is(nl_langinfo_l(CODESET, c_copy), "ASCII"));
	freelocale(c_copy);

	errno = 0;
	CHECK(duplocale(0) == 0);
	CHECK(errno == EINVAL);

	/* A thread's own locale, over the global one. */
	CHECK(uselocale(0) == LC_GLOBAL_LOCALE);
	CHECK(uselocale(utf8) == LC_GLOBAL_LOCALE);
	CHECK(uselocale(0) == utf8);
	CHECK(MB_CUR_MAX == 4);
	CHECK(is(nl_langinfo(CODESET), "UTF-8"));
	CHECK(is(setlocale(LC_ALL, NULL), "C"));
	wchar_t wc = 0;
	mbstate_t st;
	memset(&st, 0, sizeof st);
	CHECK(mbrtowc(&wc, "\xc3\xa9", 2, &st) == 2 && wc == 0xe9);

	/* setlocale changes the global locale, which the thread is not following. */
	CHECK(is(setlocale(LC_ALL, "C.UTF-8"), "C.UTF-8;C;C;C;C;C"));
	CHECK(uselocale(c) == utf8);
	CHECK(MB_CUR_MAX == 1);
	CHECK(is(nl_langinfo(CODESET), "ASCII"));
	CHECK(is(nl_langinfo_l(CODESET, LC_GLOBAL_LOCALE), "UTF-8"));
	CHECK(uselocale(LC_GLOBAL_LOCALE) == c);
	CHECK(uselocale(0) == LC_GLOBAL_LOCALE);
	CHECK(MB_CUR_MAX == 4);

	/* A copy of the global locale does not follow it. */
	locale_t global = duplocale(LC_GLOBAL_LOCALE);
	CHECK(global != 0);
	CHECK(is(nl_langinfo_l(CODESET, global), "UTF-8"));
	CHECK(is(setlocale(LC_ALL, "C"), "C"));
	CHECK(is(nl_langinfo_l(CODESET, global), "UTF-8"));
	CHECK(MB_CUR_MAX == 1);
	freelocale(global);

	/* The global locale as a base. */
	CHECK(newlocale(LC_NUMERIC_MASK, "C", LC_GLOBAL_LOCALE) == c);
	CHECK(newlocale(LC_CTYPE_MASK, "C.UTF-8", LC_GLOBAL_LOCALE) == utf8);
	CHECK(is(setlocale(LC_ALL, NULL), "C"));
	return t_status;
}
