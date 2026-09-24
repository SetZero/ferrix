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
use crate::time::{Timespec, Timeval};

/// `S_IFIFO`, from `linux/stat.h`.
const S_IFIFO: c_uint = 0o010_000;

/// C's `struct stat`, as musl's `bits/stat.h` declares it for x86-64.
///
/// musl copied it from the kernel's `struct stat` in `asm/stat.h`, replacing
/// the kernel's separate second and nanosecond fields with `struct timespec`
/// and its padding with user-space types of the same size. The assertions
/// below check it field by field against the kernel's offsets. glibc's
/// x86-64 `struct stat` is the same.
#[cfg(target_arch = "x86_64")]
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
#[cfg(target_arch = "x86_64")]
const _: () = assert!(size_of::<Stat>() == 144);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_ino) == 8);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_nlink) == 16);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_mode) == 24);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_uid) == 28);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_gid) == 32);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_rdev) == 40);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_size) == 48);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_blksize) == 56);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_blocks) == 64);
// `st_atime`, `st_atime_nsec`, and so on, in pairs.
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_atim) == 72);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_mtim) == 88);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, st_ctim) == 104);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(Stat, __unused) == 120);

/// C's `struct stat` on AArch64: the kernel's generic one from
/// `asm-generic/stat.h`, with its second and nanosecond fields as
/// `struct timespec`, as musl's and glibc's `bits/stat.h` both declare it.
/// Its link count and block size are 32 bits, and padding follows the device
/// and the block size.
#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    /// The device holding the file, C's `dev_t`.
    pub st_dev: u64,
    /// The inode number, C's `ino_t`.
    pub st_ino: u64,
    /// The type and permissions, C's `mode_t`.
    pub st_mode: c_uint,
    /// The link count, C's `nlink_t`.
    pub st_nlink: c_uint,
    /// The owner.
    pub st_uid: c_uint,
    /// The group.
    pub st_gid: c_uint,
    /// The device a special file stands for.
    pub st_rdev: u64,
    /// Padding, the kernel's `__pad1`.
    pub __pad1: u64,
    /// The size in bytes, C's `off_t`.
    pub st_size: i64,
    /// The preferred block size for I/O.
    pub st_blksize: c_int,
    /// Padding, the kernel's `__pad2`.
    pub __pad2: c_int,
    /// The 512-byte blocks allocated.
    pub st_blocks: i64,
    /// The last access.
    pub st_atim: Timespec,
    /// The last modification.
    pub st_mtim: Timespec,
    /// The last status change.
    pub st_ctim: Timespec,
    /// Reserved, the kernel's `__unused4` and `__unused5`.
    pub __unused: [c_uint; 2],
}

#[cfg(target_arch = "aarch64")]
const _: () = assert!(size_of::<Stat>() == 128);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(Stat, st_mode) == 16);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(Stat, st_rdev) == 32);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(Stat, st_size) == 48);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(Stat, st_blksize) == 56);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(Stat, st_blocks) == 64);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(Stat, st_atim) == 72);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(Stat, st_ctim) == 104);

/// C's `struct stat` on ARMv7-A, as musl's `bits/stat.h` declares it for a
/// 64-bit `time_t`: its first 104 bytes are the kernel's `struct stat64`
/// from `asm/stat.h`, which `fstatat64` writes, and the 64-bit inode and
/// times follow. The kernel's times are 32-bit and unsigned; [`widen`] copies
/// them into the `struct timespec`s after the call.
#[cfg(target_arch = "arm")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    /// The device holding the file, C's `dev_t`.
    pub st_dev: u64,
    /// Padding, the kernel's `__pad0`.
    pub __st_dev_padding: c_int,
    /// The inode number's low 32 bits, the kernel's `__st_ino`.
    pub __st_ino_truncated: c_long,
    /// The type and permissions, C's `mode_t`.
    pub st_mode: c_uint,
    /// The link count, C's `nlink_t`.
    pub st_nlink: c_uint,
    /// The owner.
    pub st_uid: c_uint,
    /// The group.
    pub st_gid: c_uint,
    /// The device a special file stands for.
    pub st_rdev: u64,
    /// Padding, the kernel's `__pad3`.
    pub __st_rdev_padding: c_int,
    /// The size in bytes, C's `off_t`.
    pub st_size: i64,
    /// The preferred block size for I/O.
    pub st_blksize: c_long,
    /// The 512-byte blocks allocated.
    pub st_blocks: i64,
    /// The kernel's 32-bit access time: seconds and nanoseconds.
    pub __st_atim32: [c_uint; 2],
    /// The kernel's 32-bit modification time.
    pub __st_mtim32: [c_uint; 2],
    /// The kernel's 32-bit status change time.
    pub __st_ctim32: [c_uint; 2],
    /// The inode number, C's `ino_t`.
    pub st_ino: u64,
    /// The last access.
    pub st_atim: Timespec,
    /// The last modification.
    pub st_mtim: Timespec,
    /// The last status change.
    pub st_ctim: Timespec,
}

#[cfg(target_arch = "arm")]
const _: () = assert!(size_of::<Stat>() == 152);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(Stat, st_mode) == 16);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(Stat, st_rdev) == 32);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(Stat, st_size) == 48);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(Stat, st_blocks) == 64);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(Stat, __st_atim32) == 72);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(Stat, st_ino) == 96);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(Stat, st_atim) == 104);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(Stat, st_ctim) == 136);

/// Fills the `struct timespec`s of a `struct stat` the kernel's `stat64`
/// wrote the first 104 bytes of. The kernel's seconds are unsigned, which
/// takes them to 2106.
///
/// # Safety
///
/// `buf` must be a `struct stat` whose first 104 bytes the kernel wrote.
#[cfg(target_arch = "arm")]
unsafe fn widen(buf: *mut Stat) {
    // SAFETY: the caller vouches for the structure, which nothing else uses.
    let st = unsafe { &mut *buf };
    let time = |[seconds, nanoseconds]: [c_uint; 2]| Timespec {
        tv_sec: i64::from(seconds),
        tv_nsec: nanoseconds as c_long,
    };
    st.st_atim = time(st.__st_atim32);
    st.st_mtim = time(st.__st_mtim32);
    st.st_ctim = time(st.__st_ctim32);
}

/// `fstatat`'s system call: the kernel's value, and `*buf` filled on success.
///
/// # Safety
///
/// As [`fstatat`].
pub(crate) unsafe fn kernel_fstatat(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut Stat,
    flags: c_int,
) -> isize {
    #[cfg(target_arch = "arm")]
    let number = nr::FSTATAT64;
    #[cfg(not(target_arch = "arm"))]
    let number = nr::NEWFSTATAT;
    // SAFETY: the kernel reads `path` and writes `buf`, as the caller vouches.
    let ret = unsafe {
        syscall::syscall4(
            number,
            dirfd as usize,
            path.addr(),
            buf.addr(),
            flags as usize,
        )
    };
    #[cfg(target_arch = "arm")]
    if ret == 0 {
        // SAFETY: the kernel wrote the structure.
        unsafe { widen(buf) };
    }
    ret
}

/// `fstat`'s system call: the kernel's value, and `*buf` filled on success.
///
/// # Safety
///
/// `buf` must be valid for a write of a `struct stat`.
pub(crate) unsafe fn kernel_fstat(fd: c_int, buf: *mut Stat) -> isize {
    #[cfg(target_arch = "arm")]
    let number = nr::FSTAT64;
    #[cfg(not(target_arch = "arm"))]
    let number = nr::FSTAT;
    // SAFETY: the kernel writes `buf`, as the caller vouches.
    let ret = unsafe { syscall::syscall2(number, fd as usize, buf.addr()) };
    #[cfg(target_arch = "arm")]
    if ret == 0 {
        // SAFETY: the kernel wrote the structure.
        unsafe { widen(buf) };
    }
    ret
}

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
    // SAFETY: the caller's contract is `kernel_fstatat`'s.
    let ret = unsafe { kernel_fstatat(dirfd, path, buf, flags) };
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
    // SAFETY: the caller's contract is `kernel_fstat`'s.
    let ret = unsafe { kernel_fstat(fd, buf) };
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

// glibc's pre-2.33 large-file names for the same four, which Chrome and
// SwiftShader, built against glibc 2.31, call. On a 64-bit architecture
// `struct stat64` is `struct stat`. ARMv7-A's glibc gives them its 32-bit
// `time_t` structure, which this library does not have, so they are not
// defined there, and such a program fails to load naming one.

/// glibc's pre-2.33 name for [`stat64`].
///
/// # Safety
///
/// As [`stat`].
#[cfg(not(target_arch = "arm"))]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __xstat64(_version: c_int, path: *const c_char, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `stat`'s.
    unsafe { stat(path, buf) }
}

/// glibc's pre-2.33 name for [`fstat64`].
///
/// # Safety
///
/// As [`fstat`].
#[cfg(not(target_arch = "arm"))]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fxstat64(_version: c_int, fd: c_int, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `fstat`'s.
    unsafe { fstat(fd, buf) }
}

/// glibc's pre-2.33 name for [`lstat64`].
///
/// # Safety
///
/// As [`lstat`].
#[cfg(not(target_arch = "arm"))]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __lxstat64(_version: c_int, path: *const c_char, buf: *mut Stat) -> c_int {
    // SAFETY: the caller's contract is `lstat`'s.
    unsafe { lstat(path, buf) }
}

/// glibc's pre-2.33 name for [`fstatat64`].
///
/// # Safety
///
/// As [`fstatat`].
#[cfg(not(target_arch = "arm"))]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fxstatat64(
    _version: c_int,
    dirfd: c_int,
    path: *const c_char,
    buf: *mut Stat,
    flags: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `fstatat`'s.
    unsafe { fstatat(dirfd, path, buf, flags) }
}

/// Changes the mode of `path` itself, not of what a symbolic link names, as
/// glibc has since 2.32: a file's mode changes, and a symbolic link, whose
/// mode Linux does not change, fails with `EOPNOTSUPP`.
///
/// That is `fchmodat` with `AT_SYMLINK_NOFOLLOW`, which is `fchmodat2`. A
/// kernel without it answers `EOPNOTSUPP` for everything, so then a path
/// that is not a symbolic link has its mode changed the plain way.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lchmod(path: *const c_char, mode: c_uint) -> c_int {
    /// `S_IFMT`, the file type bits of a mode.
    const S_IFMT: c_uint = 0o170_000;
    /// `S_IFLNK`, a symbolic link's type.
    const S_IFLNK: c_uint = 0o120_000;
    // SAFETY: the caller's contract is `fchmodat`'s.
    if unsafe { fchmodat(AT_FDCWD, path, mode, AT_SYMLINK_NOFOLLOW) } == 0 {
        return 0;
    }
    if errno::get() != errno::EOPNOTSUPP {
        return -1;
    }
    let mut st = Stat::default();
    // SAFETY: as above, and `st` is a live local.
    if unsafe { lstat(path, &raw mut st) } != 0 {
        return -1;
    }
    if st.st_mode & S_IFMT == S_IFLNK {
        errno::set(errno::EOPNOTSUPP);
        return -1;
    }
    // SAFETY: as above.
    unsafe { fchmodat(AT_FDCWD, path, mode, 0) }
}

/// Sets `path`'s access and modification times to `times[0]` and `times[1]`,
/// given in microseconds, or both to now if `times` is null. As in musl,
/// microseconds outside `0..1000000` fail with `EINVAL`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `times` null or valid for a
/// read of two `struct timeval`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn utimes(path: *const c_char, times: *const Timeval) -> c_int {
    // SAFETY: the caller's contract is `utimes_at`'s.
    unsafe { utimes_at(path, times, 0) }
}

/// Sets `path`'s times as [`utimes`] does, but on the link itself when `path`
/// names a symbolic link rather than on what it points at.
///
/// # Safety
///
/// As [`utimes`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lutimes(path: *const c_char, times: *const Timeval) -> c_int {
    // SAFETY: the caller's contract is `utimes_at`'s.
    unsafe { utimes_at(path, times, AT_SYMLINK_NOFOLLOW) }
}

/// What [`utimes`] and [`lutimes`] share: the microsecond times converted to
/// nanoseconds and given to `utimensat` with `flags`.
///
/// # Safety
///
/// As [`utimes`].
unsafe fn utimes_at(path: *const c_char, times: *const Timeval, flags: c_int) -> c_int {
    if times.is_null() {
        // SAFETY: the caller passes a NUL-terminated string.
        return unsafe { utimensat(AT_FDCWD, path, null(), flags) };
    }
    // SAFETY: the caller passes two `struct timeval`.
    let given = unsafe { times.cast::<[Timeval; 2]>().read() };
    let mut specs = [Timespec::default(); 2];
    for (spec, tv) in specs.iter_mut().zip(given) {
        if !(0..1_000_000).contains(&tv.tv_usec) {
            errno::set(errno::EINVAL);
            return -1;
        }
        *spec = Timespec {
            tv_sec: tv.tv_sec,
            // Below a second, checked above: it fits a `long`.
            tv_nsec: (tv.tv_usec * 1000) as c_long,
        };
    }
    // SAFETY: the caller passes a NUL-terminated string, and `specs` is a live
    // local.
    unsafe { utimensat(AT_FDCWD, path, specs.as_ptr(), flags) }
}

/// `struct utimbuf`, from `utime.h`: an access and a modification time in
/// whole seconds.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Utimbuf {
    /// The access time.
    pub actime: i64,
    /// The modification time.
    pub modtime: i64,
}

const _: () = assert!(size_of::<Utimbuf>() == 16);

/// Sets `path`'s access and modification times to those in `*times`, or to
/// now if `times` is null, as `utimensat` does, following symbolic links.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `times` null or valid for a
/// read of a `struct utimbuf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn utime(path: *const c_char, times: *const Utimbuf) -> c_int {
    if times.is_null() {
        // SAFETY: the caller passes a NUL-terminated string.
        return unsafe { utimensat(AT_FDCWD, path, null(), 0) };
    }
    // SAFETY: the caller vouches for a `struct utimbuf`.
    let given = unsafe { times.read() };
    let specs = [
        Timespec {
            tv_sec: given.actime,
            tv_nsec: 0,
        },
        Timespec {
            tv_sec: given.modtime,
            tv_nsec: 0,
        },
    ];
    // SAFETY: the caller passes a NUL-terminated string, and `specs` is a live
    // local.
    unsafe { utimensat(AT_FDCWD, path, specs.as_ptr(), 0) }
}
