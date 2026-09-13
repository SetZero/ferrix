//! `sys/random.h`'s `getrandom`, and `unistd.h`'s `getentropy` built on it.

use core::ffi::{c_int, c_uint, c_void};

use crate::errno;
use crate::syscall::{self, nr};

/// The most `getentropy` fills in one call.
const ENTROPY_MAX: usize = 256;

/// Fills up to `len` bytes at `buf` with random bytes, and returns how many.
///
/// # Safety
///
/// `buf` must be valid for writes of `len` bytes that nothing else refers to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getrandom(buf: *mut c_void, len: usize, flags: c_uint) -> isize {
    // SAFETY: the caller vouches for the buffer.
    let ret = unsafe { syscall::syscall3(nr::GETRANDOM, buf.addr(), len, flags as usize) };
    errno::from_syscall(ret)
}

/// Fills all `len` bytes at `buf` with random bytes. More than 256 fails with
/// `EIO`, as POSIX specifies.
///
/// Adapted from musl (MIT): short reads and interruptions are retried.
///
/// # Safety
///
/// `buf` must be valid for writes of `len` bytes that nothing else refers to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getentropy(buf: *mut c_void, len: usize) -> c_int {
    if len > ENTROPY_MAX {
        errno::set(errno::EIO);
        return -1;
    }
    let mut done = 0;
    while done < len {
        // SAFETY: `done < len`, so the rest of the caller's buffer is
        // `len - done` bytes from `buf + done`.
        let ret = unsafe {
            syscall::syscall3(
                nr::GETRANDOM,
                buf.wrapping_byte_add(done).addr(),
                len - done,
                0,
            )
        };
        match errno::decode(ret) {
            Ok(filled) => done += filled,
            Err(errno::EINTR) => {}
            Err(error) => {
                errno::set(error);
                return -1;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getentropy_fills_the_buffer_and_refuses_more_than_256_bytes() {
        let mut bytes = [0_u8; 64];
        // SAFETY: the buffer is a live local of the length given.
        let filled = unsafe { getentropy(bytes.as_mut_ptr().cast(), bytes.len()) };
        assert_eq!(filled, 0);
        assert_ne!(bytes, [0; 64]);
        // SAFETY: the call fails before writing.
        let refused = unsafe { getentropy(bytes.as_mut_ptr().cast(), ENTROPY_MAX + 1) };
        assert_eq!(refused, -1);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::EIO);
    }
}
