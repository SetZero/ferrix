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

/// Fills `n` bytes at `buf` with random bytes from the kernel, which never
/// fails: glibc's and the BSDs' `arc4random_buf`. Since glibc 2.37 it asks
/// `getrandom` every time, as this does, and a system with no source of
/// randomness at all stops the program, as glibc's does, rather than
/// return bytes that are not random. expat and libXdmcp call it.
///
/// # Safety
///
/// `buf` must be valid for writes of `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn arc4random_buf(buf: *mut c_void, n: usize) {
    let mut done = 0;
    while done < n {
        // SAFETY: the rest of the caller's buffer.
        let ret = unsafe {
            syscall::syscall3(
                nr::GETRANDOM,
                buf.wrapping_byte_add(done).addr(),
                n - done,
                0,
            )
        };
        match errno::decode(ret) {
            Ok(filled) => done += filled,
            Err(errno::EINTR) => {}
            Err(_) => crate::fortify::fortify_fail(
                b"Fatal glibc error: cannot get entropy for arc4random\n",
            ),
        }
    }
}

/// A random 32-bit number.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn arc4random() -> u32 {
    let mut word = 0_u32;
    // SAFETY: `word` is a live local of four bytes.
    unsafe { arc4random_buf((&raw mut word).cast(), 4) };
    word
}

/// A random number below `upper`, every one equally likely, or 0 for an
/// `upper` below 2. Numbers from the part of the 32-bit range that would
/// favour the low results are drawn again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn arc4random_uniform(upper: u32) -> u32 {
    if upper < 2 {
        return 0;
    }
    // 2^32 mod `upper`: below this, the results would not be even.
    let floor = upper.wrapping_neg() % upper;
    loop {
        let word = arc4random();
        if word >= floor {
            return word % upper;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc4random_fills_whole_buffers_and_keeps_to_its_bound() {
        let mut bytes = [0_u8; 1000];
        // SAFETY: a live local of the length given.
        unsafe { arc4random_buf(bytes.as_mut_ptr().cast(), bytes.len()) };
        assert!(bytes.iter().filter(|&&b| b == 0).count() < 50);
        let mut seen = [false; 7];
        for _ in 0..1000 {
            let n = arc4random_uniform(7) as usize;
            assert!(n < 7);
            if let Some(slot) = seen.get_mut(n) {
                *slot = true;
            }
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(arc4random_uniform(1), 0);
        assert_eq!(arc4random_uniform(0), 0);
    }

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
