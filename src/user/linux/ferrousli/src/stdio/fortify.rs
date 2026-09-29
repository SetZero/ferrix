//! glibc's checked `printf` entry points, which programs built with
//! `_FORTIFY_SOURCE` call instead of the plain ones.
//!
//! Each takes a flag before the format, and the string forms the size of the
//! destination as the compiler knew it, `(size_t)-1` when it did not. Output
//! that would pass that size, or an `snprintf` limit larger than it, stops the
//! program with glibc's message and `SIGABRT`.
//!
//! glibc also refuses `%n` in a format that is not read-only memory when the
//! flag asks for level 2. That check needs to know where the format lives, and
//! is not made: `%n` is accepted at every level.

use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;

use super::file::{self, File, Inner};
use super::io::{fread, read_line};
use super::printf::{vasprintf, vdprintf, vfprintf, vprintf, vsnprintf};
use crate::string::strlen;
use crate::va::{self, VaListArg};

/// Reports a buffer overflow and aborts, as glibc's `__chk_fail` does.
fn overflow() -> ! {
    crate::fortify::__chk_fail()
}

/// `vprintf`, checked.
///
/// # Safety
///
/// As `vprintf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __vprintf_chk(_flag: c_int, fmt: *const c_char, ap: VaListArg) -> c_int {
    // SAFETY: the caller vouches for the format and arguments.
    unsafe { vprintf(fmt, ap) }
}

/// `vfprintf`, checked.
///
/// # Safety
///
/// As `vfprintf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __vfprintf_chk(
    stream: *mut File,
    _flag: c_int,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller vouches for the stream, format and arguments.
    unsafe { vfprintf(stream, fmt, ap) }
}

/// `vdprintf`, checked.
///
/// # Safety
///
/// As `vdprintf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __vdprintf_chk(
    fd: c_int,
    _flag: c_int,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller vouches for the format and arguments.
    unsafe { vdprintf(fd, fmt, ap) }
}

/// `vasprintf`, checked.
///
/// # Safety
///
/// As `vasprintf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __vasprintf_chk(
    strp: *mut *mut c_char,
    _flag: c_int,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller vouches for the pointer, format and arguments.
    unsafe { vasprintf(strp, fmt, ap) }
}

/// `vsprintf` into an object of `size` bytes, aborting if the output and its
/// NUL do not fit.
///
/// # Safety
///
/// `s` must be valid for writes of `size` bytes, and the rest as `vsprintf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __vsprintf_chk(
    s: *mut c_char,
    _flag: c_int,
    size: usize,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    if size == 0 {
        overflow();
    }
    // SAFETY: the caller vouches for `size` bytes, the format and arguments.
    let written = unsafe { vsnprintf(s, size, fmt, ap) };
    if usize::try_from(written).is_ok_and(|written| written >= size) {
        overflow();
    }
    written
}

/// `vsnprintf` with a limit of `maxlen` into an object of `size` bytes,
/// aborting if the limit is larger than the object.
///
/// # Safety
///
/// As `vsnprintf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __vsnprintf_chk(
    s: *mut c_char,
    maxlen: usize,
    _flag: c_int,
    size: usize,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    if maxlen > size {
        overflow();
    }
    // SAFETY: the caller vouches for `maxlen` bytes, the format and arguments.
    unsafe { vsnprintf(s, maxlen, fmt, ap) }
}

va::variadic!(__printf_chk, 2, __vprintf_chk);
va::variadic!(__fprintf_chk, 3, __vfprintf_chk);
va::variadic!(__dprintf_chk, 3, __vdprintf_chk);
va::variadic!(__asprintf_chk, 3, __vasprintf_chk);
va::variadic!(__sprintf_chk, 4, __vsprintf_chk);
va::variadic!(__snprintf_chk, 5, __vsnprintf_chk);

/// `fread` into an object of `ptrlen` bytes, refusing a read that could pass
/// its end or whose size overflows.
///
/// # Safety
///
/// As `fread`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fread_chk(
    ptr: *mut c_void,
    ptrlen: usize,
    size: usize,
    count: usize,
    stream: *mut File,
) -> usize {
    match size.checked_mul(count) {
        Some(bytes) if bytes <= ptrlen => {}
        _ => overflow(),
    }
    // SAFETY: the caller vouches for the stream, and the read fits.
    unsafe { fread(ptr, size, count, stream) }
}

/// The body of `__fgets_chk`: `fgets` of at most `n - 1` bytes into a buffer
/// of `size`. As glibc, the line is read up to `size` bytes, and a line that
/// fills them, leaving no room for the NUL, stops the program.
///
/// # Safety
///
/// `s` must be valid for writes of `size` bytes.
unsafe fn read_line_checked(
    inner: &mut Inner,
    s: *mut c_char,
    size: usize,
    n: c_int,
) -> *mut c_char {
    let wanted = usize::try_from(n).unwrap_or(0);
    if wanted <= size {
        // SAFETY: `n` bytes fit in the caller's `size`.
        return unsafe { read_line(inner, s, n) };
    }
    // `size` is below `n`, so it fits in an `int`.
    let Ok(fits) = c_int::try_from(size) else {
        overflow()
    };
    if fits == 0 {
        return null_mut();
    }
    // SAFETY: at most `size - 1` bytes and the NUL.
    let line = unsafe { read_line(inner, s, fits) };
    if line.is_null() {
        return line;
    }
    // SAFETY: `read_line` wrote a NUL-terminated string into `s`.
    let len = unsafe { strlen(s) };
    // SAFETY: `len` is at least 1 when this reads, and `s` holds `len` bytes.
    let ended = len != 0 && unsafe { s.wrapping_add(len - 1).read() } == b'\n' as c_char;
    // A line that filled `size - 1` bytes and goes on would have been read on
    // into the NUL's place.
    if len + 1 == size && !ended && inner.peek().is_some() {
        overflow();
    }
    line
}

/// `fgets` into a buffer of `size` bytes, stopping the program if the line
/// would not fit.
///
/// # Safety
///
/// As `fgets`, with `s` valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fgets_chk(
    s: *mut c_char,
    size: usize,
    n: c_int,
    stream: *mut File,
) -> *mut c_char {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a live stream and a buffer of `size`.
        unsafe { read_line_checked(inner, s, size, n) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// `__fgets_chk` without the lock.
///
/// # Safety
///
/// As `__fgets_chk`, and the caller holds the lock or need not.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fgets_unlocked_chk(
    s: *mut c_char,
    size: usize,
    n: c_int,
    stream: *mut File,
) -> *mut c_char {
    let op = |inner: &mut Inner| {
        // SAFETY: as above.
        unsafe { read_line_checked(inner, s, size, n) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::unlocked(stream, op) }
}
