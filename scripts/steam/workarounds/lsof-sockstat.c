/*
 * Launch-side workaround for Steam on Ferrix: lsof and socket descriptors.
 *
 * Kernel gap: stat() through /proc/<pid>/fd/<n> fails with ENOENT when the
 * descriptor is a socket (kernel/src/fs/procfs.rs, descriptor_location
 * follows the link as its text, "socket:[N]", for every detached file but
 * a pipe). Linux's stat reaches the socket's sockfs inode: S_IFSOCK, and
 * the st_ino /proc/net/tcp names. lsof needs that to tie a TCP connection
 * to its process, and the Steam client runs lsof to learn which process
 * opened its UI websocket; it accepts only its own web helper's.
 * This rebuilds that stat from the link's text when the real one fails.
 *
 * Real fix: stat of a /proc/<pid>/fd link reaches the open file.
 * Owner: steam-procfs-fd. Delete this file, and scripts/steam/lsof's
 * LD_PRELOAD, when that lands (docs/STEAM.md, the workaround table).
 *
 * Built by scripts/fetch/fetch-steam-window.sh for x86-64 (lsof is
 * Debian's amd64 build), and preloaded into lsof alone by scripts/steam/lsof.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

static int fixup(const char *p, struct stat *s)
{
    char link[64];
    unsigned long ino;
    if (!p || strncmp(p, "/proc/", 6) || !strstr(p, "/fd/"))
        return 0;
    ssize_t n = readlink(p, link, sizeof link - 1);
    if (n <= 0)
        return 0;
    link[n] = 0;
    if (sscanf(link, "socket:[%lu]", &ino) != 1)
        return 0;
    struct stat own;
    memset(&own, 0, sizeof own);
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd >= 0) {
        int (*rf)(int, struct stat *) = (int (*)(int, struct stat *))dlsym(RTLD_NEXT, "fstat");
        rf(fd, &own);
        close(fd);
    }
    memset(s, 0, sizeof *s);
    s->st_dev = own.st_dev;
    s->st_ino = ino;
    s->st_mode = S_IFSOCK | 0777;
    s->st_nlink = 1;
    s->st_blksize = 4096;
    return 1;
}

#define WRAP(name, T)                                                           \
    int name(const char *p, T *s)                                               \
    {                                                                           \
        int (*r)(const char *, T *) = (int (*)(const char *, T *))dlsym(RTLD_NEXT, #name); \
        int e = r(p, s);                                                        \
        if (e < 0 && errno == ENOENT && fixup(p, (struct stat *)s)) return 0;   \
        return e;                                                               \
    }
WRAP(stat, struct stat)
WRAP(stat64, struct stat64)

#define WRAPAT(name, T)                                                         \
    int name(int d, const char *p, T *s, int f)                                 \
    {                                                                           \
        int (*r)(int, const char *, T *, int) = (int (*)(int, const char *, T *, int))dlsym(RTLD_NEXT, #name); \
        int e = r(d, p, s, f);                                                  \
        if (e < 0 && errno == ENOENT && !(f & AT_SYMLINK_NOFOLLOW) && fixup(p, (struct stat *)s)) return 0; \
        return e;                                                               \
    }
WRAPAT(fstatat, struct stat)
WRAPAT(fstatat64, struct stat64)
