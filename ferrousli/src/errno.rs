//! `errno`, and the decoding of a system call's return value into it.
//!
//! There is one `errno` for the whole process, because there are no threads
//! yet. When threads arrive it moves into the thread control block, and
//! [`__errno_location`] is already the only way C reaches it, as in glibc and
//! musl, so no program has to change.

use core::ffi::c_int;
use core::sync::atomic::{AtomicI32, Ordering};

/// `EINTR`: interrupted by a signal before anything happened.
pub const EINTR: c_int = 4;
/// `EBADF`: not an open file descriptor.
pub const EBADF: c_int = 9;

/// The process's `errno`. Atomic only so that it can be a safe `static`; C
/// writes it through the pointer without ordering anyway.
static ERRNO: AtomicI32 = AtomicI32::new(0);

/// Where `errno` lives. `<errno.h>` defines `errno` as `*__errno_location()`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __errno_location() -> *mut c_int {
    ERRNO.as_ptr()
}

/// Sets `errno`.
pub fn set(value: c_int) {
    ERRNO.store(value, Ordering::Relaxed);
}

/// Splits a raw system call return into the value or the error number.
pub fn decode(ret: isize) -> Result<usize, c_int> {
    if (-4095..0).contains(&ret) {
        // The range check makes this fit: the value is 1 to 4095.
        Err(-ret as c_int)
    } else {
        Ok(ret.cast_unsigned())
    }
}

/// C's convention for a call that returns a count: the value, or `-1` with
/// `errno` set.
pub fn from_syscall(ret: isize) -> isize {
    match decode(ret) {
        Ok(_) => ret,
        Err(error) => {
            set(error);
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_page_of_values_is_an_error() {
        assert_eq!(decode(0), Ok(0));
        assert_eq!(decode(-1), Err(1));
        assert_eq!(decode(-4095), Err(4095));
        assert_eq!(decode(-4096), Ok((-4096_isize).cast_unsigned()));
    }

    #[test]
    fn a_failed_call_returns_minus_one_and_sets_errno() {
        assert_eq!(from_syscall(-(EBADF as isize)), -1);
        // SAFETY: the pointer is to a live static.
        assert_eq!(unsafe { __errno_location().read() }, EBADF);
    }
}
