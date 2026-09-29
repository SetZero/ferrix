/*
 * wctype.h and wcwidth: musl's classes, case mappings and widths beyond
 * ASCII, which are the same in every locale, and the names wctype and
 * wctrans know.
 */

#define _GNU_SOURCE
#include <locale.h>
#include <wchar.h>
#include <wctype.h>
#include "check.h"

static void classes(void)
{
	CHECK(iswalpha(0xe9) && iswalpha(0x4e00) && iswalpha(0xaa));
	CHECK(!iswalpha('1') && !iswalpha(0x2014));
	/* musl counts all of U+20000 to U+2FFFD as alphabetic. */
	CHECK(iswalpha(0x20000) && iswalpha(0x2fffd));
	CHECK(!iswalpha(0x2fffe) && !iswalpha(0x110000) && !iswalpha(WEOF));
	CHECK(iswalnum('7') && iswalnum(0xe9) && !iswalnum('-'));

	/* Only ASCII digits. */
	CHECK(iswdigit('7') && !iswdigit(0x661) && !iswdigit(0xff11));
	CHECK(iswxdigit('F') && iswxdigit('a') && !iswxdigit('g'));
	CHECK(!iswxdigit(0xff21));

	CHECK(iswspace(0x3000) && iswspace(0x2028) && iswspace(0x85));
	CHECK(!iswspace(0xa0) && !iswspace(0x2007) && !iswspace(0x202f));
	CHECK(!iswspace(0x1680) && !iswspace(0));
	CHECK(iswblank(' ') && iswblank('\t') && !iswblank(0x3000));

	CHECK(iswcntrl(0) && iswcntrl(0x9f) && iswcntrl(0x2029));
	CHECK(iswcntrl(0xfffa) && !iswcntrl(0xa0));

	/* Every scalar value is printable but controls and noncharacters. */
	CHECK(iswprint(0xa0) && iswprint(0x378) && iswprint(0x10fffd));
	CHECK(iswprint(0xe000) && iswprint(0x20000));
	CHECK(!iswprint(0x7f) && !iswprint(0x9f) && !iswprint(0x2029));
	CHECK(!iswprint(0xfffe) && !iswprint(0x10ffff) && !iswprint(0xd800));
	CHECK(!iswprint(0x110000) && !iswprint(WEOF));
	CHECK(iswgraph('a') && iswgraph(0xa0) && !iswgraph(' '));
	CHECK(!iswgraph(0x3000));

	CHECK(iswpunct('!') && iswpunct(0x2014) && iswpunct(0x20ac));
	CHECK(!iswpunct('a') && !iswpunct(0x20000));

	CHECK(iswupper(0xc9) && !iswupper(0xe9) && iswlower(0xe9));
	CHECK(iswlower(0x3c3) && iswupper(0x3a3) && !iswlower(0x4e00));
}

static void cases(void)
{
	CHECK(towupper(0xe9) == 0xc9 && towlower(0xc9) == 0xe9);
	CHECK(towlower(0x130) == 'i');
	CHECK(towupper(0x131) == 'I');
	CHECK(towupper(0xff) == 0x178);
	/* The four title-case letters: one below in upper case, one above in lower. */
	CHECK(towupper(0x1c5) == 0x1c4 && towlower(0x1c5) == 0x1c6);
	CHECK(towupper(0x1f2) == 0x1f1 && towlower(0x1f2) == 0x1f3);
	CHECK(towupper(0xff41) == 0xff21);
	CHECK(towupper(0x10428) == 0x10400 && towlower(0x10400) == 0x10428);
	/* musl maps sharp s to capital sharp s. */
	CHECK(towupper(0xdf) == 0x1e9e && towlower(0x1e9e) == 0xdf);
	CHECK(towupper(0x4e00) == 0x4e00);
	CHECK(towupper(0x20000) == 0x20000);
	CHECK(towupper(WEOF) == WEOF && towlower(0x110000) == 0x110000);
}

static void names(void)
{
	static const char *const known[] = {
		"alnum", "alpha", "blank", "cntrl", "digit", "graph",
		"lower", "print", "punct", "space", "upper", "xdigit",
	};
	for (int i = 0; i < 12; i++)
		CHECK(wctype(known[i]) != 0);
	CHECK(iswctype(0xe9, wctype("alpha")));
	CHECK(iswctype(' ', wctype("space")));
	CHECK(!iswctype('a', wctype("digit")));
	CHECK(iswctype('f', wctype("xdigit")));
	CHECK(wctype("bogus") == 0 && wctype("") == 0 && wctype("Alpha") == 0);
	CHECK(!iswctype('a', 0) && !iswctype('a', 99));

	wctrans_t up = wctrans("toupper"), down = wctrans("tolower");
	CHECK(up != 0 && down != 0 && up != down);
	CHECK(towctrans(0xe9, up) == 0xc9 && towctrans('Q', down) == 'q');
	CHECK(wctrans("totitle") == 0);
	CHECK(towctrans('a', 0) == 'a');
}

static void widths(void)
{
	CHECK(wcwidth(0) == 0);
	CHECK(wcwidth(7) == -1 && wcwidth(0x7f) == -1 && wcwidth(0x85) == -1);
	CHECK(wcwidth('a') == 1 && wcwidth(0xe9) == 1 && wcwidth(0xa0) == 1);
	CHECK(wcwidth(0x301) == 0 && wcwidth(0x200b) == 0);
	CHECK(wcwidth(0x4e00) == 2 && wcwidth(0xff21) == 2 && wcwidth(0x1f600) == 2);
	CHECK(wcwidth(0x20000) == 2 && wcwidth(0x3fffd) == 2);
	CHECK(wcwidth(0xfffe) == -1 && wcwidth(0x1ffff) == -1);
	CHECK(wcwidth(0xe0001) == 0 && wcwidth(0xe0100) == 0);
	CHECK(wcwidth(0x378) == 1);

	CHECK(wcswidth(L"a一́", 3) == 3);
	CHECK(wcswidth(L"a一́", 2) == 3);
	CHECK(wcswidth(L"ab", 1) == 1);
	CHECK(wcswidth(L"a\x07z", 3) == -1);
	CHECK(wcswidth(L"a\0\x07", 3) == 1);
	CHECK(wcswidth(L"", 5) == 0);
}

int main(void)
{
	/* The same in the C locale and in UTF-8. */
	for (int i = 0; i < 2; i++) {
		classes();
		cases();
		names();
		widths();
		CHECK(setlocale(LC_ALL, "C.UTF-8") != NULL);
	}
	return t_status;
}
