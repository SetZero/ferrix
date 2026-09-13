//! `sys/mount.h` and `sys/swap.h`, and the calls that act on a whole file
//! system: `chroot`, `pivot_root`, `sync`, `syncfs` and `readahead`.
//!
//! Each is one system call, as in musl. musl's headers do not declare
//! `pivot_root`; a program that calls it declares it, as busybox does.

use core::ffi::{c_char, c_int, c_ulong, c_void};

use crate::errno;
use crate::syscall::{self, nr};

/// Mounts `source`, a file system of type `fstype`, on the directory
/// `target`, with the `MS_*` bits in `flags` and the file system's own
/// options in `data`.
///
/// # Safety
///
/// `target` must be a NUL-terminated string, and `source`, `fstype` and
/// `data` null or what the file system type expects of them.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mount(
    source: *const c_char,
    target: *const c_char,
    fstype: *const c_char,
    flags: c_ulong,
    data: *const c_void,
) -> c_int {
    // SAFETY: the kernel only reads the strings and `data`, which the caller
    // vouches for.
    let ret = unsafe {
        syscall::syscall6(
            nr::MOUNT,
            source.addr(),
            target.addr(),
            fstype.addr(),
            flags as usize,
            data.addr(),
            0,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Unmounts the file system mounted on `target`, with the `MNT_*` and
/// `UMOUNT_*` bits in `flags`.
///
/// # Safety
///
/// `target` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn umount2(target: *const c_char, flags: c_int) -> c_int {
    // SAFETY: the kernel only reads `target`.
    let ret = unsafe { syscall::syscall2(nr::UMOUNT2, target.addr(), flags as usize) };
    errno::from_syscall(ret) as c_int
}

/// Unmounts the file system mounted on `target`.
///
/// # Safety
///
/// `target` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn umount(target: *const c_char) -> c_int {
    // SAFETY: the caller's contract is `umount2`'s.
    unsafe { umount2(target, 0) }
}

/// Makes the directory `new_root` the root file system, and moves the old one
/// to `put_old`.
///
/// # Safety
///
/// Both paths must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pivot_root(new_root: *const c_char, put_old: *const c_char) -> c_int {
    // SAFETY: the kernel only reads the two paths.
    let ret = unsafe { syscall::syscall2(nr::PIVOT_ROOT, new_root.addr(), put_old.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Makes the directory `path` the calling process's root directory.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn chroot(path: *const c_char) -> c_int {
    // SAFETY: the kernel only reads `path`.
    let ret = unsafe { syscall::syscall2(nr::CHROOT, path.addr(), 0) };
    errno::from_syscall(ret) as c_int
}

/// Starts swapping to the file or device `path`, with the `SWAP_FLAG_*` bits
/// in `flags`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn swapon(path: *const c_char, flags: c_int) -> c_int {
    // SAFETY: the kernel only reads `path`.
    let ret = unsafe { syscall::syscall2(nr::SWAPON, path.addr(), flags as usize) };
    errno::from_syscall(ret) as c_int
}

/// Stops swapping to the file or device `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn swapoff(path: *const c_char) -> c_int {
    // SAFETY: the kernel only reads `path`.
    let ret = unsafe { syscall::syscall2(nr::SWAPOFF, path.addr(), 0) };
    errno::from_syscall(ret) as c_int
}

/// Schedules every file system's cached writes to storage. It cannot fail.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sync() {
    // SAFETY: `sync` reads no memory.
    let _ = unsafe { syscall::syscall0(nr::SYNC) };
}

/// Writes the cached data of the file system holding `fd` to storage.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn syncfs(fd: c_int) -> c_int {
    // SAFETY: `syncfs` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::SYNCFS, fd as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// Reads `count` bytes of `fd` from `offset` into the page cache, ahead of
/// their use.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn readahead(fd: c_int, offset: i64, count: usize) -> isize {
    // SAFETY: `readahead` reads no memory.
    let ret = unsafe { syscall::syscall3(nr::READAHEAD, fd as usize, offset as usize, count) };
    errno::from_syscall(ret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last_errno() -> c_int {
        // SAFETY: the pointer is this thread's errno.
        unsafe { errno::__errno_location().read() }
    }

    #[test]
    fn each_call_reaches_the_kernel_with_its_arguments_in_place() {
        // The kernel looks the target up before it checks for privilege, so
        // this is `ENOENT` whoever runs it.
        // SAFETY: every string is NUL-terminated.
        let ret = unsafe {
            mount(
                c"none".as_ptr(),
                c"/nonexistent/ferrousli".as_ptr(),
                c"tmpfs".as_ptr(),
                0,
                core::ptr::null(),
            )
        };
        assert_eq!(ret, -1);
        assert_eq!(last_errno(), errno::ENOENT);
        // SAFETY: the path is NUL-terminated.
        assert_eq!(unsafe { umount2(c".".as_ptr(), 0x7fff_0000) }, -1);
        assert_eq!(last_errno(), errno::EINVAL);
        // SAFETY: as above.
        assert_eq!(unsafe { chroot(c"/nonexistent/ferrousli".as_ptr()) }, -1);
        assert_eq!(last_errno(), errno::ENOENT);
        assert_eq!(syncfs(-1), -1);
        assert_eq!(last_errno(), errno::EBADF);
        assert_eq!(readahead(-1, 0, 4096), -1);
        assert_eq!(last_errno(), errno::EBADF);
        sync();
    }
}
