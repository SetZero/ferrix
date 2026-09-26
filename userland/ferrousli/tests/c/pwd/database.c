/*
 * The user and group databases, read from the system's own /etc/passwd,
 * /etc/group and /etc/shadow, in which every Linux has a root entry; the
 * login name; and the shells. Nothing outside the test's directory is
 * changed, whoever runs it.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <grp.h>
#include <limits.h>
#include <pwd.h>
#include <shadow.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "check.h"

int main(void)
{
	struct passwd *pw, pwbuf, *pwres;
	struct group *gr, grbuf, *grres;
	struct spwd sp, *spres;
	char buf[4096], tiny[4], name[LOGIN_NAME_MAX], group_name[64] = "";
	gid_t groups[64];
	int n, found, rv;
	char *shell;

	/* Users, by name and by id, statically and reentrantly. */
	pw = getpwnam("root");
	CHECK(pw && pw->pw_uid == 0 && !strcmp(pw->pw_name, "root") && pw->pw_dir[0] == '/');
	pw = getpwuid(0);
	CHECK(pw && !strcmp(pw->pw_name, "root"));
	CHECK(!getpwnam("ferrousli-nobody"));
	CHECK(getpwnam_r("root", &pwbuf, buf, sizeof buf, &pwres) == 0 && pwres == &pwbuf);
	CHECK(pwbuf.pw_uid == 0 && pwbuf.pw_name == buf && pwbuf.pw_shell < buf + sizeof buf);
	CHECK(!strcmp(pwbuf.pw_name, "root"));
	CHECK(getpwuid_r(0, &pwbuf, tiny, sizeof tiny, &pwres) == ERANGE && !pwres);
	CHECK(getpwnam_r("ferrousli-nobody", &pwbuf, buf, sizeof buf, &pwres) == 0 && !pwres);

	/* Every entry once, and again after endpwent. */
	found = 0;
	setpwent();
	while ((pw = getpwent()))
		if (pw->pw_uid == 0 && !strcmp(pw->pw_name, "root"))
			found++;
	CHECK(found == 1);
	endpwent();
	pw = getpwent();
	CHECK(pw != 0);
	endpwent();

	/* Groups. */
	gr = getgrgid(0);
	CHECK(gr && gr->gr_gid == 0 && gr->gr_mem);
	if (gr && strlen(gr->gr_name) < sizeof group_name)
		strcpy(group_name, gr->gr_name);
	gr = getgrnam(group_name);
	CHECK(gr && gr->gr_gid == 0);
	CHECK(getgrgid_r(0, &grbuf, buf, sizeof buf, &grres) == 0 && grres == &grbuf);
	CHECK(grbuf.gr_gid == 0 && grbuf.gr_mem && !strcmp(grbuf.gr_name, group_name));
	CHECK((char *)grbuf.gr_mem >= buf && (char *)grbuf.gr_mem < buf + sizeof buf);
	CHECK(getgrnam_r(group_name, &grbuf, tiny, sizeof tiny, &grres) == ERANGE && !grres);
	found = 0;
	setgrent();
	while ((gr = getgrent()))
		if (gr->gr_gid == 0)
			found++;
	endgrent();
	CHECK(found >= 1);

	/* The group list starts with the given group, and says how many there
	 * are when they do not fit. */
	n = 64;
	CHECK(getgrouplist("root", 0, groups, &n) >= 1 && n >= 1 && groups[0] == 0);
	n = 0;
	CHECK(getgrouplist("root", 0, groups, &n) == -1 && n >= 1);
	if (geteuid() != 0) {
		errno = 0;
		CHECK(initgroups("root", 0) == -1 && errno == EPERM);
	}

	/* The shadow entry, which only root may read. */
	rv = getspnam_r("root", &sp, buf, sizeof buf, &spres);
	CHECK(rv == 0 ? spres == &sp && !strcmp(sp.sp_namp, "root") : rv == EACCES && !spres);
	CHECK(getspnam_r("../root", &sp, buf, sizeof buf, &spres) == EINVAL && !spres);
	CHECK(getspnam_r("root", &sp, tiny, sizeof tiny, &spres) == ERANGE);

	/* Shells. */
	setusershell();
	shell = getusershell();
	CHECK(shell && shell[0] == '/');
	endusershell();
	shell = getusershell();
	CHECK(shell && shell[0] == '/');
	endusershell();

	/* The login name comes from LOGNAME. */
	unsetenv("LOGNAME");
	CHECK(!getlogin() && getlogin_r(name, sizeof name) == ENXIO);
	CHECK(setenv("LOGNAME", "someone", 1) == 0);
	CHECK(getlogin() && !strcmp(getlogin(), "someone"));
	CHECK(getlogin_r(name, sizeof name) == 0 && !strcmp(name, "someone"));
	CHECK(getlogin_r(name, 7) == ERANGE);
	return t_status;
}
