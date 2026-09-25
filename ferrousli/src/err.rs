//! `err.h`: the BSD calls that report a failure on standard error with the
//! program's name in front, and `err` and `errx` that then exit.
//!
//! `warn(fmt, ...)` writes `name: message: text of errno`, `warnx` the same
//! without the text of `errno`; a null format leaves the message and its
//! `": "` out. util-linux's libraries, which Chrome loads through GLib, call
//! them.

use core::ffi::{CStr, c_char, c_int};

use crate::stdio::file::{self, Inner};
use crate::stdio::printf::{FileSink, Sink, vfprintf_locked};
use crate::va::{self, VaListArg};
use crate::{errno, exit, gnu, strerror};

/// Writes `name: `, the formatted message, and with `with_errno` the text of
/// `error` after `": "`, then a newline, to standard error, under its lock so
/// that another thread's output does not come between.
///
/// # Safety
///
/// `fmt` must be null, or a format whose conversions `ap`'s arguments match.
unsafe fn report(fmt: *const c_char, ap: VaListArg, with_errno: bool, error: c_int) {
    let name = gnu::short_name();
    let write = |inner: &mut Inner| {
        // A sink gathers what it is given, so each is flushed before the
        // stream is written another way.
        let mut sink = FileSink::new(inner);
        if !name.is_null() {
            // SAFETY: the name is `argv[0]`'s tail, a NUL-terminated string.
            sink.write(unsafe { CStr::from_ptr(name) }.to_bytes());
        }
        sink.write(b": ");
        sink.flush();
        if !fmt.is_null() {
            // SAFETY: the caller vouches for the format and its list.
            let _ = unsafe { vfprintf_locked(inner, fmt, ap) };
        }
        let mut sink = FileSink::new(inner);
        if !fmt.is_null() && with_errno {
            sink.write(b": ");
        }
        if with_errno {
            // SAFETY: `strerror` returns a static string.
            sink.write(unsafe { CStr::from_ptr(strerror::strerror(error)) }.to_bytes());
        }
        sink.write(b"\n");
        sink.flush();
    };
    let err = file::stderr.load(core::sync::atomic::Ordering::Relaxed);
    // SAFETY: standard error is a static stream.
    unsafe { file::locked(err, write) };
}

/// This thread's `errno`.
fn current_errno() -> c_int {
    // SAFETY: the pointer is this thread's `errno`.
    unsafe { errno::__errno_location().read() }
}

/// Reports the message and the text of `errno` on standard error.
///
/// # Safety
///
/// `fmt` must be null, or a format whose conversions `ap`'s arguments match.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vwarn(fmt: *const c_char, ap: VaListArg) {
    let error = current_errno();
    // SAFETY: the caller's contract.
    unsafe { report(fmt, ap, true, error) };
}

/// Reports the message on standard error.
///
/// # Safety
///
/// As [`vwarn`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vwarnx(fmt: *const c_char, ap: VaListArg) {
    // SAFETY: the caller's contract.
    unsafe { report(fmt, ap, false, 0) };
}

/// [`vwarn`], then `exit(status)`.
///
/// # Safety
///
/// As [`vwarn`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn verr(status: c_int, fmt: *const c_char, ap: VaListArg) -> ! {
    // SAFETY: the caller's contract.
    unsafe { vwarn(fmt, ap) };
    exit::exit(status)
}

/// [`vwarnx`], then `exit(status)`.
///
/// # Safety
///
/// As [`vwarn`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn verrx(status: c_int, fmt: *const c_char, ap: VaListArg) -> ! {
    // SAFETY: the caller's contract.
    unsafe { vwarnx(fmt, ap) };
    exit::exit(status)
}

va::variadic!(warn, 1, vwarn);
va::variadic!(warnx, 1, vwarnx);
va::variadic!(err, 2, verr);
va::variadic!(errx, 2, verrx);
