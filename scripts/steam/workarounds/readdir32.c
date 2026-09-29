/*
 * Launch-side workaround for Steam on Ferrix: 32-bit readdir on /proc.
 *
 * Kernel gap: Ferrix numbers /proc's inodes with the pid in bits 32-63
 * (kernel/src/fs/procfs.rs, Place::ino), so they do not fit the 32-bit
 * d_ino of an i386 program's non-LFS struct dirent, and glibc's readdir()
 * fails with EOVERFLOW on /proc. The Steam client lists /proc that way to
 * find the web helper's processes; with an empty list its UI transport
 * rejects the web helper's websocket ("Checked: <pid>/<pid>", then
 * "Connection rejected", then "Unexpected Transport Error 0x3008").
 * This reads the 64-bit entry and folds its inode number into 32 bits.
 * Linux's /proc inode numbers fit 32 bits.
 *
 * Real fix: 32-bit /proc inode numbers. Owner: steam-procfs-fd.
 * Delete this file when that lands (docs/STEAM.md, the workaround table).
 *
 * Built by scripts/fetch/fetch-steam-window.sh for i386 and x86-64
 * (-nostdlib), and preloaded by scripts/steam/client.sh through $LIB. Only
 * i386 has the short struct dirent, so the x86-64 build is empty: it is
 * there so the dynamic loader finds a file at the same $LIB path.
 */
#ifdef __i386__
struct dirent32 {
    unsigned int d_ino;
    int d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[256];
};
struct dirent64 {
    unsigned long long d_ino;
    long long d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[256];
};
struct dirent64 *readdir64(void *dir);

static __thread struct dirent32 folded;

struct dirent32 *readdir(void *dir)
{
    struct dirent64 *entry = readdir64(dir);
    if (!entry)
        return 0;
    folded.d_ino = (unsigned int)(entry->d_ino ^ (entry->d_ino >> 32));
    if (folded.d_ino == 0)
        folded.d_ino = 1;
    folded.d_off = (int)entry->d_off;
    folded.d_reclen = sizeof folded;
    folded.d_type = entry->d_type;
    int i = 0;
    for (; i < 255 && entry->d_name[i]; i++)
        folded.d_name[i] = entry->d_name[i];
    folded.d_name[i] = 0;
    return &folded;
}
#endif
