//! `sys/stat.h`: file status, permissions, and creating directories and
//! special files.
//!
//! Each path call is its `*at` form relative to `AT_FDCWD`, which is all
//! AArch64 has. glibc's pre-2.33 `__xstat` family, which older glibc binaries
//! call in place of `stat`, is exported beside the `*64` names.

use core::ffi::{c_char, c_int, c_long, c_uint};
use core::mem::{offset_of, size_of};
use core::ptr::null;

use crate::errno;
use crate::fcntl::{AT_FDCWD, AT_SYMLINK_NOFOLLOW};
use crate::syscall::{self, nr};
use crate::time::Timespec;

/// `S_IFIFO`, from `linux/stat.h`.
const S_IFIFO: c_uint = 0o010_000;

/// C's `struct stat`, as musl's `bits/stat.h` declares it for x86-64.
///
/// musl copied it from the kernel's `struct stat` in `asm/stat.h`, replacing
/// the kernel's separate second and nanosecond fields with `struct timespec`
/// and its padding with user-space types of the same size. The assertions
/// below check it field by field against the kernel's offsets. glibc's
/// x86-64 `struct stat` is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    /// The device holding the file, C's `dev_t`.
    pub st_dev: u64,
    /// The inode number, C's `ino_t`.
    pub st_ino: u64,
    /// The link count, C's `nlink_t`.
    pub st_nlink: u64,
    /// The type and permissions, C's `mode_t`.
    pub st_mode: c_uint,
    /// The owner.
    pub st_uid: c_uint,
    /// The group.
    pub st_gid: c_uint,
    /// Padding, the kernel's `__pad0`.
    pub __pad0: c_uint,
    /// The device a special file stands for.
    pub st_rdev: u64,
    /// The size in bytes, C's `off_t`.
    pub st_size: i64,
    /// The preferred block size for I/O.
    pub st_blksize: c_long,
    /// The 512-byte blocks allocated.
    pub st_blocks: i64,
    /// The last access.
    pub st_atim: Timespec,
    /// The last modification.
    pub st_mtim: Timespec,
    /// The last status change.
    pub st_ctim: Timespec,
    /// Reserved, the kernel's `__unused`.
    pub __unused: [c_long; 3],
}

// The kernel's `struct stat` on x86-64, from `asm/stat.h`: three unsigned
// longs, four unsigned ints, then eleven longs and three reserved ones.
const _: () = assert!(size_of::<Stat>() == 144);
const _: () = assert!(offset_of!(Stat, st_ino) == 8);
const _: () = assert!(offset_of!(Stat, st_nlink) == 16);
const _: () = assert!(offset_of!(Stat, st_mode) == 24);
const _: () = assert!(offset_of!(Stat, st_uid) == 28);
const _: () = assert!(offset_of!(Stat, st_gid) == 32);
const _: () = assert!(offset_of!(Stat, st_rdev) == 40);
const _: () = assert!(offset_of!(Stat, st_size) == 48);
const _: () = assert!(offset_of!(Stat, st_blksize) == 56);
const _: () = assert!(offset_of!(Stat, st_blocks) == 64);
// `st_atime`, `st_atime_nsec`, and so on, in pairs.
const _: () = assert!(offset_of!(Stat, st_atim) == 72);
const _: () = assert!(offset_of!(Stat, st_mtim) == 88);
const _: () = assert!(offset_of!(Stat, st_ctim) == 104);
const _: () = assert!(offset_of!(Stat, __unused) == 120);

/// The status of `path`, relative to `dirfd`, into `*buf`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `buf` valid for a write of a
/// `struct stat`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstatat(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut Stat,
    flags: c_int,
) -> c_int {
    // SAFETY: the kernel reads `path` and writes `buf`, as the caller vouches.
    let ret = unsafe {
        syscall::syscall4(
            nr::NEWFSTATAT,
            dirfd as usize,
            path.addr(),
            buf.addr(),
            flags as usize,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// The status of `path`, following a final symbolic link, into `*buf`.
///
/// # Safety
///
/// As [`fstatat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn stat(path: *const c_char, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `fstatat`'s.
    unsafe { fstatat(AT_FDCWD, path, buf, 0) }
}

/// The status of `path`, not following a final symbolic link, into `*buf`.
///
/// # Safety
///
/// As [`fstatat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lstat(path: *const c_char, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `fstatat`'s.
    unsafe { fstatat(AT_FDCWD, path, buf, AT_SYMLINK_NOFOLLOW) }
}

/// The status of the file `fd` refers to, into `*buf`.
///
/// # Safety
///
/// `buf` must be valid for a write of a `struct stat`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstat(fd: c_int, buf: *mut Stat) -> c_int {
    // SAFETY: the kernel writes `buf`, as the caller vouches.
    let ret = unsafe { syscall::syscall2(nr::FSTAT, fd as usize, buf.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Creates the directory `path`, relative to `dirfd`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkdirat(dirfd: c_int, path: *const c_char, mode: c_uint) -> c_int {
    // SAFETY: the kernel only reads `path`.
    let ret = unsafe { syscall::syscall3(nr::MKDIRAT, dirfd as usize, path.addr(), mode as usize) };
    errno::from_syscall(ret) as c_int
}

/// Creates the directory `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkdir(path: *const c_char, mode: c_uint) -> c_int {
    // SAFETY: the caller's contract is `mkdirat`'s.
    unsafe { mkdirat(AT_FDCWD, path, mode) }
}

/// Sets the permissions of `path`, relative to `dirfd`.
///
/// `AT_SYMLINK_NOFOLLOW` needs the `fchmodat2` system call, from Linux 6.6.
/// On an older kernel it fails with `EOPNOTSUPP`; musl's fallback through
/// `/proc/self/fd` is not here.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fchmodat(
    dirfd: c_int,
    path: *const c_char,
    mode: c_uint,
    flags: c_int,
) -> c_int {
    if flags == 0 {
        // SAFETY: the kernel only reads `path`.
        let ret =
            unsafe { syscall::syscall3(nr::FCHMODAT, dirfd as usize, path.addr(), mode as usize) };
        return errno::from_syscall(ret) as c_int;
    }
    // SAFETY: as above.
    let ret = unsafe {
        syscall::syscall4(
            nr::FCHMODAT2,
            dirfd as usize,
            path.addr(),
            mode as usize,
            flags as usize,
        )
    };
    match errno::decode(ret) {
        Ok(_) => 0,
        Err(errno::ENOSYS) => {
            errno::set(if flags == AT_SYMLINK_NOFOLLOW {
                errno::EOPNOTSUPP
            } else {
                errno::EINVAL
            });
            -1
        }
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// Sets the permissions of `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn chmod(path: *const c_char, mode: c_uint) -> c_int {
    // SAFETY: the caller's contract is `fchmodat`'s.
    unsafe { fchmodat(AT_FDCWD, path, mode, 0) }
}

/// Sets the permissions of the file `fd` refers to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fchmod(fd: c_int, mode: c_uint) -> c_int {
    // SAFETY: `fchmod` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::FCHMOD, fd as usize, mode as usize) };
    errno::from_syscall(ret) as c_int
}

/// Sets the file mode creation mask, and returns the previous one. It cannot
/// fail.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn umask(mask: c_uint) -> c_uint {
    // SAFETY: `umask` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::UMASK, (mask & 0o777) as usize, 0) };
    ret as c_uint
}

/// Creates a special file, relative to `dirfd`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mknodat(
    dirfd: c_int,
    path: *const c_char,
    mode: c_uint,
    dev: u64,
) -> c_int {
    // SAFETY: the kernel only reads `path`. It reads the low 32 bits of
    // `dev`, the only ones its device number encoding has.
    let ret = unsafe {
        syscall::syscall4(
            nr::MKNODAT,
            dirfd as usize,
            path.addr(),
            mode as usize,
            dev as usize,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Creates a special file.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mknod(path: *const c_char, mode: c_uint, dev: u64) -> c_int {
    // SAFETY: the caller's contract is `mknodat`'s.
    unsafe { mknodat(AT_FDCWD, path, mode, dev) }
}

/// Creates a FIFO, relative to `dirfd`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkfifoat(dirfd: c_int, path: *const c_char, mode: c_uint) -> c_int {
    // SAFETY: the caller's contract is `mknodat`'s.
    unsafe { mknodat(dirfd, path, mode | S_IFIFO, 0) }
}

/// Creates a FIFO.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkfifo(path: *const c_char, mode: c_uint) -> c_int {
    // SAFETY: the caller's contract is `mkfifoat`'s.
    unsafe { mkfifoat(AT_FDCWD, path, mode) }
}

/// Sets the access and modification times of `path`, relative to `dirfd`, or
/// of `dirfd` itself if `path` is null. Null `times` means now.
///
/// # Safety
///
/// `path` must be null or a NUL-terminated string, and `times` null or valid
/// for a read of two `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn utimensat(
    dirfd: c_int,
    path: *const c_char,
    times: *const Timespec,
    flags: c_int,
) -> c_int {
    // SAFETY: the kernel only reads `path` and `times`.
    let ret = unsafe {
        syscall::syscall4(
            nr::UTIMENSAT,
            dirfd as usize,
            path.addr(),
            times.addr(),
            flags as usize,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Sets the access and modification times of the file `fd` refers to.
///
/// # Safety
///
/// `times` must be null or valid for a read of two `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn futimens(fd: c_int, times: *const Timespec) -> c_int {
    // SAFETY: a null path makes the call act on `fd`; the caller vouches for
    // `times`.
    unsafe { utimensat(fd, null(), times, 0) }
}

/// glibc's large-file name for [`stat`].
///
/// # Safety
///
/// As [`stat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn stat64(path: *const c_char, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `stat`'s.
    unsafe { stat(path, buf) }
}

/// glibc's large-file name for [`fstat`].
///
/// # Safety
///
/// As [`fstat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstat64(fd: c_int, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `fstat`'s.
    unsafe { fstat(fd, buf) }
}

/// glibc's large-file name for [`lstat`].
///
/// # Safety
///
/// As [`lstat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lstat64(path: *const c_char, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `lstat`'s.
    unsafe { lstat(path, buf) }
}

/// glibc's large-file name for [`fstatat`].
///
/// # Safety
///
/// As [`fstatat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstatat64(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut Stat,
    flags: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `fstatat`'s.
    unsafe { fstatat(dirfd, path, buf, flags) }
}

/// glibc's pre-2.33 name for [`stat`]. The version, 1 on x86-64, is ignored:
/// there is one `struct stat`.
///
/// # Safety
///
/// As [`stat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __xstat(_version: c_int, path: *const c_char, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `stat`'s.
    unsafe { stat(path, buf) }
}

/// glibc's pre-2.33 name for [`fstat`]. The version is ignored.
///
/// # Safety
///
/// As [`fstat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fxstat(_version: c_int, fd: c_int, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `fstat`'s.
    unsafe { fstat(fd, buf) }
}

/// glibc's pre-2.33 name for [`lstat`]. The version is ignored.
///
/// # Safety
///
/// As [`lstat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __lxstat(_version: c_int, path: *const c_char, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `lstat`'s.
    unsafe { lstat(path, buf) }
}

/// glibc's pre-2.33 name for [`fstatat`]. The version is ignored.
///
/// # Safety
///
/// As [`fstatat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fxstatat(
    _version: c_int,
    dirfd: c_int,
    path: *const c_char,
    buf: *mut Stat,
    flags: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `fstatat`'s.
    unsafe { fstatat(dirfd, path, buf, flags) }
}
