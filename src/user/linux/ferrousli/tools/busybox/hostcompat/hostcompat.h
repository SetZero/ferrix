/* Forced into every busybox host-program compile under mingw by build-windows.sh. */
#ifndef HOSTCOMPAT_H
#define HOSTCOMPAT_H
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <io.h>
#include <direct.h>
#include <process.h>

/* Binary files: kbuild's shell steps choke on CRLF. */
static void __attribute__((constructor)) hostcompat_binmode(void)
{
	_fmode = _O_BINARY;
}

#define mkdir(path, mode) _mkdir(path)
#define pipe(fds) _pipe((fds), 4096, _O_BINARY)
#define random rand
#define srandom srand

/* mingw's popen runs cmd.exe; kbuild's commands are POSIX shell. */
static FILE *hostcompat_popen(const char *cmd, const char *mode)
{
	char script[1024], line[1200];
	const char *tmp = getenv("TEMP");
	static int n;
	snprintf(script, sizeof script, "%s/hostcompat-popen-%d-%d.sh", tmp ? tmp : ".", _getpid(), n++);
	FILE *f = fopen(script, "wb");
	if (!f)
		return NULL;
	fputs(cmd, f);
	fputc('\n', f);
	fclose(f);
	snprintf(line, sizeof line, "bash \"%s\"", script);
	return _popen(line, mode);
}
/* Windows cannot rename an open file or onto an existing one. applet_tables
 * dup2()s its output to fd 1 and closes only stdout, leaving the first
 * descriptor open; host programs exit right after renaming, so on failure
 * close the stray descriptors, remove the target and try again. */
static void hostcompat_ignore_invalid(const wchar_t *e, const wchar_t *f, const wchar_t *file,
				      unsigned int line, uintptr_t reserved)
{
	(void)e; (void)f; (void)file; (void)line; (void)reserved;
}

static int hostcompat_rename(const char *from, const char *to)
{
	if (rename(from, to) == 0)
		return 0;
	/* Closing a descriptor that is not open must not abort. */
	_set_invalid_parameter_handler(hostcompat_ignore_invalid);
	for (int fd = 3; fd < 256; fd++)
		_close(fd);
	remove(to);
	return rename(from, to);
}
#define rename hostcompat_rename

#undef popen
#undef pclose
#define popen hostcompat_popen
#define pclose _pclose

#endif
