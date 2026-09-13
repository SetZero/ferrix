//! `sys/statfs.h` and `sys/statvfs.h`: how large a file system is and how
//! full.
//!
//! `statvfs` is `statfs` with its fields rearranged, as in musl.

use core::ffi::{c_char, c_int, c_uint, c_ulong};
use core::mem::{offset_of, size_of};

use crate::errno;
use crate::syscall::{self, nr};

/// C's `struct statfs`, from musl's `bits/statfs.h`.
///
/// The kernel's `struct statfs` in `asm-generic/statfs.h`, whose
/// `__statfs_word` is a `long` on a 64-bit architecture, has the same layout,
/// and so has glibc's.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Statfs {
    /// The file system's magic number.
    pub f_type: c_ulong,
    /// The block size for I/O.
    pub f_bsize: c_ulong,
    /// Blocks in all.
    pub f_blocks: u64,
    /// Free blocks.
    pub f_bfree: u64,
    /// Free blocks an unprivileged user may use.
    pub f_bavail: u64,
    /// Inodes in all.
    pub f_files: u64,
    /// Free inodes.
    pub f_ffree: u64,
    /// The file system's id, `fsid_t`.
    pub f_fsid: [c_int; 2],
    /// The longest name a directory entry may have.
    pub f_namelen: c_ulong,
    /// The fragment size.
    pub f_frsize: c_ulong,
    /// The `ST_*` mount flags.
    pub f_flags: c_ulong,
    /// Room the kernel reserves.
    pub f_spare: [c_ulong; 4],
}

const _: () = assert!(offset_of!(Statfs, f_fsid) == 56);
const _: () = assert!(offset_of!(Statfs, f_namelen) == 64);
const _: () = assert!(offset_of!(Statfs, f_spare) == 88);
const _: () = assert!(size_of::<Statfs>() == 120);

/// C's `struct statvfs`, from musl's `sys/statvfs.h`.
///
/// On a 64-bit little-endian target its zero-width bit-field is empty. glibc's
/// has the same layout up to `f_namemax`, and six spare ints where musl keeps
/// `f_type` and five, so either reads the other's fields the same way.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Statvfs {
    /// The block size for I/O.
    pub f_bsize: c_ulong,
    /// The fragment size, the unit of the block counts.
    pub f_frsize: c_ulong,
    /// Blocks in all.
    pub f_blocks: u64,
    /// Free blocks.
    pub f_bfree: u64,
    /// Free blocks an unprivileged user may use.
    pub f_bavail: u64,
    /// Inodes in all.
    pub f_files: u64,
    /// Free inodes.
    pub f_ffree: u64,
    /// Free inodes an unprivileged user may use.
    pub f_favail: u64,
    /// The file system's id.
    pub f_fsid: c_ulong,
    /// The `ST_*` mount flags.
    pub f_flag: c_ulong,
    /// The longest name a directory entry may have.
    pub f_namemax: c_ulong,
    /// The file system's magic number, which musl adds.
    pub f_type: c_uint,
    /// Reserved.
    pub __reserved: [c_int; 5],
}

const _: () = assert!(offset_of!(Statvfs, f_fsid) == 64);
const _: () = assert!(offset_of!(Statvfs, f_flag) == 72);
const _: () = assert!(offset_of!(Statvfs, f_type) == 88);
const _: () = assert!(offset_of!(Statvfs, __reserved) == 92);
const _: () = assert!(size_of::<Statvfs>() == 112);

/// Describes the file system holding `path` into `*buf`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `buf` valid for a write of a
/// `struct statfs`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn statfs(path: *const c_char, buf: *mut Statfs) -> c_int {
    // The kernel leaves spare fields alone, so they are cleared first, as
    // musl does.
    // SAFETY: the caller vouches for `buf`.
    unsafe { buf.write(Statfs::default()) };
    // SAFETY: the kernel reads `path` and writes `buf`, as the caller vouches.
    let ret = unsafe { syscall::syscall2(nr::STATFS, path.addr(), buf.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Describes the file system holding `fd` into `*buf`.
///
/// # Safety
///
/// `buf` must be valid for a write of a `struct statfs`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstatfs(fd: c_int, buf: *mut Statfs) -> c_int {
    // SAFETY: the caller vouches for `buf`.
    unsafe { buf.write(Statfs::default()) };
    // SAFETY: the kernel writes `buf`, as the caller vouches.
    let ret = unsafe { syscall::syscall2(nr::FSTATFS, fd as usize, buf.addr()) };
    errno::from_syscall(ret) as c_int
}

/// `statvfs`'s fields from `statfs`'s, as musl fills them. A zero fragment
/// size is the block size, and `f_favail` is `f_ffree`, since Linux reserves
/// no inodes. The id is the first half of `fsid_t`, sign-extended as C's
/// assignment extends it.
fn to_statvfs(info: &Statfs) -> Statvfs {
    Statvfs {
        f_bsize: info.f_bsize,
        f_frsize: if info.f_frsize != 0 {
            info.f_frsize
        } else {
            info.f_bsize
        },
        f_blocks: info.f_blocks,
        f_bfree: info.f_bfree,
        f_bavail: info.f_bavail,
        f_files: info.f_files,
        f_ffree: info.f_ffree,
        f_favail: info.f_ffree,
        f_fsid: info.f_fsid[0] as c_ulong,
        f_flag: info.f_flags,
        f_namemax: info.f_namelen,
        f_type: info.f_type as c_uint,
        __reserved: [0; 5],
    }
}

/// Describes the file system holding `path` into `*buf`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `buf` valid for a write of a
/// `struct statvfs`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn statvfs(path: *const c_char, buf: *mut Statvfs) -> c_int {
    let mut info = Statfs::default();
    // SAFETY: the caller vouches for `path`, and `info` is a live local.
    if unsafe { statfs(path, &raw mut info) } != 0 {
        return -1;
    }
    // SAFETY: the caller vouches for `buf`.
    unsafe { buf.write(to_statvfs(&info)) };
    0
}

/// Describes the file system holding `fd` into `*buf`.
///
/// # Safety
///
/// `buf` must be valid for a write of a `struct statvfs`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstatvfs(fd: c_int, buf: *mut Statvfs) -> c_int {
    let mut info = Statfs::default();
    // SAFETY: `info` is a live local.
    if unsafe { fstatfs(fd, &raw mut info) } != 0 {
        return -1;
    }
    // SAFETY: the caller vouches for `buf`.
    unsafe { buf.write(to_statvfs(&info)) };
    0
}

/// glibc's large-file name for [`statfs`].
///
/// # Safety
///
/// As [`statfs`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn statfs64(path: *const c_char, buf: *mut Statfs) -> c_int {
    // SAFETY: the caller's contract is `statfs`'s.
    unsafe { statfs(path, buf) }
}

/// glibc's large-file name for [`fstatfs`].
///
/// # Safety
///
/// As [`fstatfs`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstatfs64(fd: c_int, buf: *mut Statfs) -> c_int {
    // SAFETY: the caller's contract is `fstatfs`'s.
    unsafe { fstatfs(fd, buf) }
}

/// glibc's large-file name for [`statvfs`].
///
/// # Safety
///
/// As [`statvfs`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn statvfs64(path: *const c_char, buf: *mut Statvfs) -> c_int {
    // SAFETY: the caller's contract is `statvfs`'s.
    unsafe { statvfs(path, buf) }
}

/// glibc's large-file name for [`fstatvfs`].
///
/// # Safety
///
/// As [`fstatvfs`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstatvfs64(fd: c_int, buf: *mut Statvfs) -> c_int {
    // SAFETY: the caller's contract is `fstatvfs`'s.
    unsafe { fstatvfs(fd, buf) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statvfs_takes_the_block_size_for_a_missing_fragment_size() {
        let info = Statfs {
            f_type: 0x9fa0,
            f_bsize: 4096,
            f_blocks: 10,
            f_ffree: 7,
            f_fsid: [-2, 5],
            f_namelen: 255,
            f_flags: 1,
            ..Statfs::default()
        };
        let out = to_statvfs(&info);
        assert_eq!(out.f_frsize, 4096);
        assert_eq!(out.f_favail, 7);
        assert_eq!(out.f_fsid, (-2_i64) as c_ulong);
        assert_eq!((out.f_namemax, out.f_flag, out.f_type), (255, 1, 0x9fa0));
        let out = to_statvfs(&Statfs {
            f_frsize: 1024,
            ..info
        });
        assert_eq!(out.f_frsize, 1024);
    }

    #[test]
    fn the_working_directory_s_file_system_is_described_both_ways() {
        let mut info = Statfs::default();
        let mut vfs = Statvfs::default();
        // SAFETY: the path is NUL-terminated and the structures live locals.
        assert_eq!(unsafe { statfs(c".".as_ptr(), &raw mut info) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { statvfs(c".".as_ptr(), &raw mut vfs) }, 0);
        assert!(info.f_bsize > 0 && info.f_namelen > 0);
        assert_eq!((vfs.f_bsize, vfs.f_namemax), (info.f_bsize, info.f_namelen));
        // SAFETY: as above.
        assert_eq!(unsafe { fstatvfs(-1, &raw mut vfs) }, -1);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::EBADF);
    }
}
