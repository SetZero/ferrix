//! `unistd.h`: the calls that are each one system call.

use core::ffi::{c_int, c_void};

use crate::syscall::{self, nr};
use crate::{errno, exit};

/// Reads up to `count` bytes from `fd` into `buf`.
///
/// # Safety
///
/// `buf` must be valid for writes of `count` bytes that nothing else refers
/// to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize {
    // SAFETY: the caller vouches for the buffer. Casting `fd` sign-extends,
    // and the kernel reads the low 32 bits back.
    let ret = unsafe { syscall::syscall3(nr::READ, fd as usize, buf.addr(), count) };
    errno::from_syscall(ret)
}

/// Writes up to `count` bytes from `buf` to `fd`.
///
/// # Safety
///
/// `buf` must be valid for reads of `count` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: usize) -> isize {
    // SAFETY: the caller vouches for the buffer, and the kernel only reads it.
    let ret = unsafe { syscall::syscall3(nr::WRITE, fd as usize, buf.addr(), count) };
    errno::from_syscall(ret)
}

/// Ends the process with `status` at once. POSIX's name for `_Exit`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn _exit(status: c_int) -> ! {
    exit::_Exit(status)
}
