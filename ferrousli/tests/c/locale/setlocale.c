/*
 * setlocale, localeconv and nl_langinfo: musl's two built-in locales, the
 * names it accepts for any other, LC_ALL's composite names, and glibc's ctype
 * tables, which stay the C locale's under UTF-8.
 */

#define _GNU_SOURCE
#include <ctype.h>
#include <langinfo.h>
#include <limits.h>
#include <locale.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

/* glibc's accessor, which musl's headers do not declare. */
const unsigned short **__ctype_b_loc(void);

static int is(const char *got, const char *want)
{
	return got && !strcmp(got, want);
}

int main(void)
{
	/* A program starts in C. */
	CHECK(is(setlocale(LC_ALL, NULL), "C"));
	CHECK(is(setlocale(LC_CTYPE, NULL), "C"));
	CHECK(MB_CUR_MAX == 1);
	CHECK(is(nl_langinfo(CODESET), "ASCII"));

	/* Categories that do not exist. */
	CHECK(setlocale(LC_ALL + 1, "C") == NULL);
	CHECK(setlocale(-1, NULL) == NULL);
	CHECK(is(setlocale(LC_ALL, NULL), "C"));

	/* C.UTF-8 is UTF-8 in LC_CTYPE, and C in every other category. */
	CHECK(is(setlocale(LC_ALL, "C.UTF-8"), "C.UTF-8;C;C;C;C;C"));
	CHECK(is(setlocale(LC_CTYPE, NULL), "C.UTF-8"));
	CHECK(is(setlocale(LC_NUMERIC, NULL), "C"));
	CHECK(MB_CUR_MAX == 4);
	CHECK(is(nl_langinfo(CODESET), "UTF-8"));
	CHECK(is(nl_langinfo(NL_LOCALE_NAME(LC_CTYPE)), "C.UTF-8"));
	CHECK(is(nl_langinfo(NL_LOCALE_NAME(LC_TIME)), "C"));
	CHECK(is(setlocale(LC_NUMERIC, "C.UTF-8"), "C"));

	/*
	 * A composite name is accepted back. A shorter list gives its last
	 * name to the remaining categories.
	 */
	CHECK(is(setlocale(LC_ALL, "C;C;C;C;C;C"), "C"));
	CHECK(MB_CUR_MAX == 1);
	CHECK(is(setlocale(LC_ALL, "C.UTF-8;C;C;C;C;C"), "C.UTF-8;C;C;C;C;C"));
	CHECK(MB_CUR_MAX == 4);
	CHECK(is(setlocale(LC_ALL, "C;en_GB.UTF-8"),
		"C;en_GB.UTF-8;en_GB.UTF-8;en_GB.UTF-8;en_GB.UTF-8;en_GB.UTF-8"));
	CHECK(MB_CUR_MAX == 1);
	CHECK(is(setlocale(LC_MONETARY, NULL), "en_GB.UTF-8"));
	CHECK(is(setlocale(LC_ALL, "POSIX"), "C"));
	CHECK(MB_CUR_MAX == 1);

	/* Any other name is kept, the same map each time, and is UTF-8. */
	char *name = setlocale(LC_CTYPE, "en_US.UTF-8");
	CHECK(is(name, "en_US.UTF-8"));
	CHECK(setlocale(LC_CTYPE, "en_US.UTF-8") == name);
	CHECK(MB_CUR_MAX == 4);
	CHECK(is(nl_langinfo(CODESET), "UTF-8"));
	CHECK(is(setlocale(LC_ALL, NULL), "en_US.UTF-8;C;C;C;C;C"));
	CHECK(is(setlocale(LC_ALL, "ja_JP.eucJP"), "ja_JP.eucJP"));
	CHECK(MB_CUR_MAX == 4);
	CHECK(is(setlocale(LC_MESSAGES, NULL), "ja_JP.eucJP"));
	CHECK(is(nl_langinfo(NL_LOCALE_NAME(LC_MESSAGES)), "ja_JP.eucJP"));

	/* Names that cannot be names are C.UTF-8. */
	CHECK(is(setlocale(LC_CTYPE, "../../etc/passwd"), "C.UTF-8"));
	CHECK(is(setlocale(LC_CTYPE, "en/US"), "C.UTF-8"));
	CHECK(is(setlocale(LC_CTYPE, ".UTF-8"), "C.UTF-8"));
	CHECK(is(setlocale(LC_CTYPE, "abcdefghijklmnopqrstuvwx"), "C.UTF-8"));
	CHECK(is(setlocale(LC_CTYPE, "abcdefghijklmnopqrstuvw"), "abcdefghijklmnopqrstuvw"));
	CHECK(is(setlocale(LC_CTYPE, "C"), "C"));
	CHECK(MB_CUR_MAX == 1);
	CHECK(is(setlocale(LC_CTYPE, "c.utf-8"), "c.utf-8"));
	CHECK(MB_CUR_MAX == 4);

	/* With nothing in the environment, "" is C.UTF-8. */
	CHECK(is(setlocale(LC_ALL, ""), "C.UTF-8;C;C;C;C;C"));

	/* The conventions are POSIX's in every locale. */
	struct lconv *lc = localeconv();
	CHECK(is(lc->decimal_point, "."));
	CHECK(is(lc->thousands_sep, ""));
	CHECK(is(lc->grouping, ""));
	CHECK(is(lc->int_curr_symbol, ""));
	CHECK(is(lc->currency_symbol, ""));
	CHECK(is(lc->mon_decimal_point, ""));
	CHECK(is(lc->mon_thousands_sep, ""));
	CHECK(is(lc->mon_grouping, ""));
	CHECK(is(lc->positive_sign, ""));
	CHECK(is(lc->negative_sign, ""));
	CHECK(lc->int_frac_digits == CHAR_MAX);
	CHECK(lc->frac_digits == CHAR_MAX);
	CHECK(lc->p_cs_precedes == CHAR_MAX);
	CHECK(lc->n_sep_by_space == CHAR_MAX);
	CHECK(lc->int_p_sep_by_space == CHAR_MAX);
	CHECK(lc->int_n_sign_posn == CHAR_MAX);
	CHECK(localeconv() == lc);

	/* The C locale's strings. */
	CHECK(is(nl_langinfo(ABDAY_1), "Sun"));
	CHECK(is(nl_langinfo(ABDAY_7), "Sat"));
	CHECK(is(nl_langinfo(DAY_1), "Sunday"));
	CHECK(is(nl_langinfo(DAY_4), "Wednesday"));
	CHECK(is(nl_langinfo(ABMON_1), "Jan"));
	CHECK(is(nl_langinfo(ABMON_12), "Dec"));
	CHECK(is(nl_langinfo(MON_9), "September"));
	CHECK(is(nl_langinfo(MON_12), "December"));
	CHECK(is(nl_langinfo(AM_STR), "AM"));
	CHECK(is(nl_langinfo(PM_STR), "PM"));
	CHECK(is(nl_langinfo(D_T_FMT), "%a %b %e %T %Y"));
	CHECK(is(nl_langinfo(D_FMT), "%m/%d/%y"));
	CHECK(is(nl_langinfo(T_FMT), "%H:%M:%S"));
	CHECK(is(nl_langinfo(T_FMT_AMPM), "%I:%M:%S %p"));
	CHECK(is(nl_langinfo(ERA), ""));
	CHECK(is(nl_langinfo(ERA_D_FMT), "%m/%d/%y"));
	CHECK(is(nl_langinfo(ALT_DIGITS), "0123456789"));
	CHECK(is(nl_langinfo(ERA_T_FMT), "%H:%M:%S"));
	CHECK(is(nl_langinfo(RADIXCHAR), "."));
	CHECK(is(nl_langinfo(THOUSEP), ""));
	CHECK(is(nl_langinfo(YESEXPR), "^[yY]"));
	CHECK(is(nl_langinfo(NOEXPR), "^[nN]"));
	CHECK(is(nl_langinfo(YESSTR), "yes"));
	CHECK(is(nl_langinfo(NOSTR), "no"));
	CHECK(is(nl_langinfo(CRNCYSTR), ""));
	CHECK(is(nl_langinfo(ERA_T_FMT + 1), ""));
	CHECK(is(nl_langinfo(0), ""));
	CHECK(is(nl_langinfo(-1), ""));
	CHECK(is(nl_langinfo(0x7fffffff), ""));

	/* glibc's byte tables stay the C locale's under UTF-8. */
	CHECK(is(setlocale(LC_ALL, "C.UTF-8"), "C.UTF-8;C;C;C;C;C"));
	CHECK((*__ctype_b_loc())[0xe9] == 0);
	CHECK((*__ctype_b_loc())['a'] != 0);
	CHECK(!isalpha(0xe9));
	CHECK(toupper(0xe9) == 0xe9);
	return t_status;
}
