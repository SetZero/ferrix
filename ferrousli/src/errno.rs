//! `errno`, and the decoding of a system call's return value into it.
//!
//! Each thread's `errno` lives in its control block, and [`__errno_location`]
//! finds it through the thread pointer. C reaches `errno` only through that
//! function, as with glibc and musl. In unit tests the thread pointer belongs
//! to the host's C library, so a Rust thread-local stands in.

use core::ffi::c_int;

// The error numbers, generated from the kernel's headers by `tools/gen-abi.py`.
include!("generated/errno.rs");

/// `ENOTSUP`: not supported. C names it separately from `EOPNOTSUPP`, and
/// Linux gives both the same number.
pub const ENOTSUP: c_int = EOPNOTSUPP;

#[cfg(test)]
std::thread_local! {
    static ERRNO: core::cell::Cell<c_int> = const { core::cell::Cell::new(0) };
}

/// Where the calling thread's `errno` lives. `<errno.h>` defines `errno` as
/// `*__errno_location()`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __errno_location() -> *mut c_int {
    crate::thread::errno_location()
}

/// Where the calling thread's `errno` lives: in unit tests, a Rust
/// thread-local.
#[cfg(test)]
pub extern "C" fn __errno_location() -> *mut c_int {
    ERRNO.with(core::cell::Cell::as_ptr)
}

/// Sets the calling thread's `errno`.
pub fn set(value: c_int) {
    // SAFETY: the pointer is the calling thread's `errno`, which lives as
    // long as the thread.
    unsafe { __errno_location().write(value) }
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
        // SAFETY: the pointer is this thread's `errno`.
        assert_eq!(unsafe { __errno_location().read() }, EBADF);
    }
}
