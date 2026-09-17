//! glibc's fortified memory functions: `__memcpy_chk` and its relatives.
//!
//! A program compiled against glibc's headers with `_FORTIFY_SOURCE` set — as
//! every Debian and Ubuntu package is, and as `cc -O2` is on those hosts by
//! default — has its calls to `memcpy`, `memmove` and `memset` rewritten to
//! these, with one extra argument: how large the compiler proved the
//! destination to be, or `(size_t)-1` when it could not tell. Each checks the
//! length against it and stops the program when the copy would overrun.
//!
//! musl has none of them, because musl's own headers never rewrite anything.
//! They are here for the same reason `crt1.o` calls `__libc_start_main` with
//! glibc's arguments: an object file compiled against glibc must link. The
//! spike that linked uutils/coreutils against this library found
//! `__memcpy_chk` coming out of a C dependency built with the host's headers.
//!
//! Only the three memory functions are here. The string and `printf` families
//! (`__strcpy_chk`, `__sprintf_chk` and the rest) are not, and each will be
//! added the same way when something needs it.
//!
//! # What a failed check does
//!
//! glibc calls `__chk_fail`, which raises `SIGABRT` after writing a message.
//! So does this, through [`crate::signal::abort`]. The check has found a
//! buffer overrun that has not happened yet; there is nothing to return.

use core::ffi::{c_int, c_void};

use crate::signal::abort;
use crate::string::{memcpy, memmove, memset};

/// Stops the program: a fortified call was asked to write more than its
/// destination holds.
///
/// glibc exports this, and a program compiled against its headers can call it
/// directly, so it keeps the name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __chk_fail() -> ! {
    abort()
}

/// `memcpy`, refusing a copy larger than the destination.
///
/// # Safety
///
/// As `memcpy`: `dest` valid for writes of `n` bytes, `src` for reads of `n`,
/// and the two not overlapping.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __memcpy_chk(
    dest: *mut c_void,
    src: *const c_void,
    n: usize,
    destlen: usize,
) -> *mut c_void {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `memcpy`'s, and the copy fits.
    unsafe { memcpy(dest, src, n) }
}

/// `memmove`, refusing a copy larger than the destination.
///
/// # Safety
///
/// As `memmove`: both pointers valid for `n` bytes, overlapping allowed.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __memmove_chk(
    dest: *mut c_void,
    src: *const c_void,
    n: usize,
    destlen: usize,
) -> *mut c_void {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `memmove`'s, and the copy fits.
    unsafe { memmove(dest, src, n) }
}

/// `memset`, refusing a fill larger than the destination.
///
/// # Safety
///
/// As `memset`: `s` valid for writes of `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __memset_chk(
    s: *mut c_void,
    c: c_int,
    n: usize,
    destlen: usize,
) -> *mut c_void {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `memset`'s, and the fill fits.
    unsafe { memset(s, c, n) }
}
