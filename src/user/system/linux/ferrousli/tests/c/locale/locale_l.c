/*
 * The _l forms musl declares, in C, C.UTF-8 and the global locale. None of
 * the classes, case mappings, orders or messages depends on the locale, so
 * each must agree with its plain form.
 */

#define _GNU_SOURCE
#include <ctype.h>
#include <errno.h>
#include <locale.h>
#include <string.h>
#include <wchar.h>
#include <wctype.h>
#include "check.h"

static int (*const volatile classify[])(int) = {
	isalnum, isalpha, isblank, iscntrl, isdigit, isgraph, islower, isprint,
	ispunct, isspace, isupper, isxdigit, tolower, toupper,
};

static int (*const volatile classify_l[])(int, locale_t) = {
	isalnum_l, isalpha_l, isblank_l, iscntrl_l, isdigit_l, isgraph_l,
	islower_l, isprint_l, ispunct_l, isspace_l, isupper_l, isxdigit_l,
	tolower_l, toupper_l,
};

static int (*const volatile wide[])(wint_t) = {
	iswalnum, iswalpha, iswblank, iswcntrl, iswdigit, iswgraph, iswlower,
	iswprint, iswpunct, iswspace, iswupper, iswxdigit,
};

static int (*const volatile wide_l[])(wint_t, locale_t) = {
	iswalnum_l, iswalpha_l, iswblank_l, iswcntrl_l, iswdigit_l, iswgraph_l,
	iswlower_l, iswprint_l, iswpunct_l, iswspace_l, iswupper_l, iswxdigit_l,
};

static const wint_t samples[] = {
	0, '\t', ' ', '0', 'A', 'a', 0x7f, 0x85, 0xa0, 0xc9, 0xe9, 0x1c5, 0x3000,
	0x4e00, 0xfff9, 0x1f600, 0x10ffff, 0x110000, WEOF,
};

int main(void)
{
	locale_t locales[] = {
		newlocale(LC_ALL_MASK, "C", 0),
		newlocale(LC_ALL_MASK, "C.UTF-8", 0),
		LC_GLOBAL_LOCALE,
	};
	char buf[8];
	wchar_t wbuf[4];

	for (int i = 0; i < 3; i++) {
		locale_t l = locales[i];
		CHECK(l != 0);
		for (int f = 0; f < 14; f++)
			for (int ch = -1; ch < 256; ch++)
				if (classify_l[f](ch, l) != classify[f](ch)) {
					CHECK(classify_l[f](ch, l) == classify[f](ch));
					return t_status;
				}
		for (int f = 0; f < 12; f++)
			for (unsigned s = 0; s < sizeof samples / sizeof *samples; s++)
				CHECK(wide_l[f](samples[s], l) == wide[f](samples[s]));
		for (unsigned s = 0; s < sizeof samples / sizeof *samples; s++) {
			CHECK(towlower_l(samples[s], l) == towlower(samples[s]));
			CHECK(towupper_l(samples[s], l) == towupper(samples[s]));
		}
		CHECK(towupper_l(0xe9, l) == 0xc9);
		CHECK(iswctype_l(0x4e00, wctype_l("alpha", l), l));
		CHECK(!iswctype_l('1', wctype_l("alpha", l), l));
		CHECK(wctype_l("nope", l) == 0);
		CHECK(towctrans_l('a', wctrans_l("toupper", l), l) == 'A');
		CHECK(towctrans_l(0xc9, wctrans_l("tolower", l), l) == 0xe9);
		CHECK(wctrans_l("title", l) == 0);

		CHECK(strcoll_l("abc", "abd", l) < 0);
		CHECK(strcoll_l("b", "a", l) > 0);
		CHECK(strcoll_l("same", "same", l) == 0);
		CHECK(strcoll_l("\xc3\xa9", "f", l) > 0);
		CHECK(strxfrm_l(buf, "hello", sizeof buf, l) == 5 && !strcmp(buf, "hello"));
		CHECK(strxfrm_l(buf, "much too long", sizeof buf, l) == 13);
		CHECK(!strcmp(strerror_l(ENOENT, l), strerror(ENOENT)));
		CHECK(!strcmp(strerror_l(EILSEQ, l), "Illegal byte sequence"));

		CHECK(wcscasecmp_l(L"ÉtÉ", L"éTé", l) == 0);
		CHECK(wcsncasecmp_l(L"ABc", L"abD", 2, l) == 0);
		CHECK(wcsncasecmp_l(L"ABc", L"abD", 3, l) < 0);
		CHECK(wcscoll_l(L"a", L"b", l) < 0);
		CHECK(wcsxfrm_l(wbuf, L"xyz", 4, l) == 3 && !wcscmp(wbuf, L"xyz"));
	}
	return t_status;
}
