/*
 * Launch-side workaround for Steam on Ferrix: pipe2(O_DIRECT).
 *
 * Kernel gap: Ferrix has no packet-mode pipes, so pipe2() with O_DIRECT is
 * EINVAL. The 32-bit client's controllerxinput_linux.cpp asks for one and
 * fatal-asserts when it fails. This retries the call without O_DIRECT,
 * which gives a byte-stream pipe; the client works with that.
 *
 * Real fix: packet-mode pipes in the kernel. Owner: steam-pipe-direct.
 * Delete this file when that lands (docs/STEAM.md, the workaround table).
 *
 * Built by tools/common/fetch/fetch-steam-window.sh for i386 and x86-64, and
 * preloaded by tools/common/steam/client.sh. No libc headers: -nostdlib, so
 * one command line builds both without a multilib toolchain.
 */
void *dlsym(void *handle, const char *symbol);
int dprintf(int fd, const char *format, ...);
int getpid(void);
int *__errno_location(void);
#define RTLD_NEXT ((void *)-1l)
#define O_DIRECT 040000

int pipe2(int fds[2], int flags)
{
    int (*real)(int *, int) = (int (*)(int *, int))dlsym(RTLD_NEXT, "pipe2");
    int ret = real(fds, flags);
    if (ret != 0 && (flags & O_DIRECT) && *__errno_location() == 22) {
        ret = real(fds, flags & ~O_DIRECT);
        dprintf(2, "steam-workaround: pid %d pipe2 without O_DIRECT -> %d\n", getpid(), ret);
    }
    return ret;
}
