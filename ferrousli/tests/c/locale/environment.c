/*
 * setlocale(LC_ALL, "") takes each category's name from LC_ALL, then the
 * category's own LC_* variable, then LANG, and C.UTF-8 when none is set and
 * non-empty. The test runs this with an environment, and passes the name
 * setlocale should return and the digit MB_CUR_MAX should then be.
 */

#include <langinfo.h>
#include <locale.h>
#include <stdlib.h>
#include <string.h>
#include "check.h"

int main(int argc, char **argv)
{
	CHECK(argc == 3);
	if (argc != 3)
		return t_status;

	/* The environment changes nothing until setlocale is asked. */
	CHECK(MB_CUR_MAX == 1);
	CHECK(!strcmp(setlocale(LC_ALL, NULL), "C"));

	const char *name = setlocale(LC_ALL, "");
	CHECK(name != NULL && !strcmp(name, argv[1]));
	CHECK(!strcmp(setlocale(LC_ALL, NULL), argv[1]));
	CHECK(MB_CUR_MAX == (size_t)(argv[2][0] - '0'));
	CHECK(!strcmp(nl_langinfo(CODESET), MB_CUR_MAX == 4 ? "UTF-8" : "ASCII"));
	return t_status;
}
