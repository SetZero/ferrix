//! `sys/ioctl.h`: device control.
//!
//! `ioctl` is variadic in C; [`crate::fcntl`] says why it is defined with a
//! fixed third parameter.

use core::ffi::{c_int, c_ulong};

use crate::errno;
use crate::syscall::{self, nr};

/// Performs device request `request` on `fd`, passing `arg` through.
///
/// musl declares the request `int` and glibc `unsigned long`. The kernel
/// reads it as an `unsigned int`, so it is passed zero-extended and either
/// declaration reaches it the same way.
///
/// # Safety
///
/// For a request that takes a pointer, `arg` must be valid for what the
/// request reads and writes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ioctl(fd: c_int, request: c_int, arg: c_ulong) -> c_int {
    // SAFETY: the caller vouches for `arg` for this request.
    let ret = unsafe {
        syscall::syscall3(
            nr::IOCTL,
            fd as usize,
            request.cast_unsigned() as usize,
            arg as usize,
        )
    };
    errno::from_syscall(ret) as c_int
}
