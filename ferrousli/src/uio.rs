//! `sys/uio.h`: reading and writing several buffers in one call.

use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};

use crate::errno;
use crate::syscall::nr;

/// C's `struct iovec`, one buffer. The kernel's in `linux/uio.h` is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Iovec {
    /// The buffer's start.
    pub iov_base: *mut c_void,
    /// The buffer's length.
    pub iov_len: usize,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Iovec>() == 16);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Iovec, iov_len) == 4);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Iovec>() == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Iovec, iov_len) == 8);

/// Reads from `fd` into the `count` buffers at `iov`, in order.
///
/// # Safety
///
/// `iov` must be valid for reads of `count` `struct iovec`, each naming a
/// buffer valid for writes that nothing else refers to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn readv(fd: c_int, iov: *const Iovec, count: c_int) -> isize {
    // SAFETY: the caller vouches for the vector and its buffers. A negative
    // count sign-extends to a length the kernel refuses.
    let ret = unsafe {
        crate::cancel::syscall_cp(nr::READV, fd as usize, iov.addr(), count as usize, 0, 0, 0)
    };
    errno::from_syscall(ret)
}

/// Writes the `count` buffers at `iov` to `fd`, in order.
///
/// # Safety
///
/// `iov` must be valid for reads of `count` `struct iovec`, each naming a
/// buffer valid for reads.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn writev(fd: c_int, iov: *const Iovec, count: c_int) -> isize {
    // SAFETY: the caller vouches for the vector, and the kernel only reads.
    let ret = unsafe {
        crate::cancel::syscall_cp(nr::WRITEV, fd as usize, iov.addr(), count as usize, 0, 0, 0)
    };
    errno::from_syscall(ret)
}

/// [`readv`] at `offset`, leaving the file offset alone.
///
/// # Safety
///
/// As [`readv`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn preadv(fd: c_int, iov: *const Iovec, count: c_int, offset: i64) -> isize {
    // SAFETY: as in `readv`. The kernel takes the offset as a low and a high
    // half; on a 64-bit kernel the low half is the whole offset and the high
    // one is ignored, and musl passes both the same way.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::PREADV,
            fd as usize,
            iov.addr(),
            count as usize,
            offset as usize,
            (offset >> 32) as usize,
            0,
        )
    };
    errno::from_syscall(ret)
}

/// [`writev`] at `offset`, leaving the file offset alone.
///
/// # Safety
///
/// As [`writev`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pwritev(fd: c_int, iov: *const Iovec, count: c_int, offset: i64) -> isize {
    // SAFETY: as in `writev` and `preadv`.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::PWRITEV,
            fd as usize,
            iov.addr(),
            count as usize,
            offset as usize,
            (offset >> 32) as usize,
            0,
        )
    };
    errno::from_syscall(ret)
}
