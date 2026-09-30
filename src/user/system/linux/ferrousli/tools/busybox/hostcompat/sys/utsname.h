/* uname for kconfig's symbol.c, which only reads the release. */
#ifndef HOSTCOMPAT_SYS_UTSNAME_H
#define HOSTCOMPAT_SYS_UTSNAME_H
#include <string.h>

struct utsname {
	char sysname[65], nodename[65], release[65], version[65], machine[65];
};

static inline int uname(struct utsname *u)
{
	memset(u, 0, sizeof *u);
	strcpy(u->sysname, "Windows");
	strcpy(u->release, "0");
	strcpy(u->machine, "x86_64");
	return 0;
}
#endif
