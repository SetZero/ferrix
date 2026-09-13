//! `stdio.h`, as far as it goes without buffering: `puts`.
//!
//! `puts` writes straight to the descriptor. Buffered `FILE` streams come
//! with `printf`, and `puts` moves onto them then.

use core::ffi::{c_char, c_int};
use core::slice;

use crate::syscall::{self, nr};
use crate::{errno, string};

/// `EOF`, the error return of the character and line functions.
pub const EOF: c_int = -1;

/// Standard output's descriptor.
const STDOUT: c_int = 1;

/// Writes `s` and a newline to standard output.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn puts(s: *const c_char) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { string::strlen(s) };
    // SAFETY: `strlen` just found `len` readable bytes before the NUL.
    let text = unsafe { slice::from_raw_parts(s.cast::<u8>(), len) };
    match write_all(STDOUT, text).and_then(|()| write_all(STDOUT, b"\n")) {
        Ok(()) => 0,
        Err(()) => EOF,
    }
}

/// Writes all of `bytes`, retrying short writes and interruptions. On failure
/// `errno` says why.
fn write_all(fd: c_int, mut bytes: &[u8]) -> Result<(), ()> {
    while !bytes.is_empty() {
        // SAFETY: `bytes` is a live slice, and the kernel only reads it.
        let ret = unsafe {
            syscall::syscall3(nr::WRITE, fd as usize, bytes.as_ptr().addr(), bytes.len())
        };
        match errno::decode(ret) {
            // A write that makes no progress would loop forever.
            Ok(0) => return Err(()),
            Ok(written) => bytes = bytes.get(written..).unwrap_or_default(),
            Err(errno::EINTR) => {}
            Err(error) => {
                errno::set(error);
                return Err(());
            }
        }
    }
    Ok(())
}
