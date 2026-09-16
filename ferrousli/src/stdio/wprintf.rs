//! The `wprintf` family: `fwprintf`, `swprintf`, `wprintf` and their `v`
//! forms.
//!
//! The format grammar, the arguments and every number are [`super::printf`]'s,
//! as musl 1.2.5's `stdio/vfwprintf.c` (MIT) formats numbers with the narrow
//! machinery. The wide format is handed to it as a narrowed copy with the same
//! indices: each ASCII character as its byte, and any other as a byte that
//! starts no directive. The sink knows where that copy lies, so the text it is
//! given from there is written as the wide format's own characters, and text
//! from anywhere else, a number or `(null)`, is ASCII. `%c` and `%s` are the
//! engine's wide branch, counting characters. So a width, a precision, `%n`
//! and the result all count wide characters.
//!
//! As in musl, `swprintf` fails with -1 when the output with its NUL does not
//! fit, rather than returning the length it would have had, and a stream
//! written this way becomes wide-oriented.

use core::ffi::{c_char, c_int};

use core::sync::atomic::Ordering;

use super::file::{self, File, Inner};
use super::printf::{FileSink, Sink, format};
use crate::multibyte::{MbState, WChar, wcrtomb};
use crate::va::{self, VaListTag};
use crate::wchar::wcslen;
use crate::{errno, malloc};

/// Where wide output goes.
trait WideTarget {
    /// Takes `wcs`.
    fn put(&mut self, wcs: &[WChar]);
    /// Whether output has failed.
    fn failed(&self) -> bool;
}

/// The sink the engine writes to: literal text by index from the wide format,
/// anything else as ASCII.
struct WideOut<'t> {
    /// The narrowed format the engine reads.
    narrow: *const u8,
    /// Its length, not counting the NUL.
    len: usize,
    /// The wide format it stands for.
    wide: *const WChar,
    /// Where the characters go.
    target: &'t mut dyn WideTarget,
}

impl Sink for WideOut<'_> {
    fn write(&mut self, bytes: &[u8]) {
        let offset = bytes.as_ptr().addr().wrapping_sub(self.narrow.addr());
        let mut chunk = [0 as WChar; 64];
        let mut done = 0;
        while done < bytes.len() {
            let take = (bytes.len() - done).min(chunk.len());
            for (i, slot) in chunk.iter_mut().take(take).enumerate() {
                *slot = if offset < self.len && offset + bytes.len() <= self.len {
                    // SAFETY: the range lies inside the narrowed format, whose
                    // indices are the wide format's.
                    unsafe { self.wide.wrapping_add(offset + done + i).read() }
                } else {
                    WChar::from(bytes.get(done + i).copied().unwrap_or(0))
                };
            }
            self.target.put(chunk.get(..take).unwrap_or_default());
            done += take;
        }
    }

    fn failed(&self) -> bool {
        self.target.failed()
    }

    fn wide(&self) -> bool {
        true
    }

    fn write_wide(&mut self, wcs: &[i32]) {
        self.target.put(wcs);
    }
}

/// Formats the wide format `fmt` with the arguments in `ap` into `target`.
/// Returns the count of wide characters, or the error number to set, zero if
/// it is already set.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated wide string, and `ap` a `va_list` whose
/// arguments match it.
unsafe fn format_wide(
    target: &mut dyn WideTarget,
    fmt: *const WChar,
    ap: *mut VaListTag,
) -> Result<usize, c_int> {
    // SAFETY: the caller passes a NUL-terminated wide string.
    let len = unsafe { wcslen(fmt) };
    let narrow = malloc::malloc(len + 1).cast::<u8>();
    if narrow.is_null() {
        return Err(errno::ENOMEM);
    }
    for i in 0..=len {
        // SAFETY: `i` is at most `len`, inside the format and its NUL.
        let wc = unsafe { fmt.wrapping_add(i).read() };
        let byte = u8::try_from(wc).ok().filter(u8::is_ascii).unwrap_or(0x80);
        // SAFETY: `narrow` has room for `len + 1` bytes.
        unsafe { narrow.wrapping_add(i).write(byte) };
    }
    let mut out = WideOut {
        narrow,
        len,
        wide: fmt,
        target,
    };
    // SAFETY: `narrow` is NUL-terminated, and the caller vouches for the
    // arguments, which match the directives it keeps.
    let result = unsafe { format(&mut out, narrow.cast::<c_char>(), ap) };
    // SAFETY: `narrow` came from `malloc` and is not used again.
    unsafe { malloc::free(narrow.cast()) };
    result
}

/// C's return for a formatting result: the count, or -1 with `errno` set.
fn finish(result: Result<usize, c_int>) -> c_int {
    match result {
        Ok(count) => c_int::try_from(count).unwrap_or(c_int::MAX),
        Err(error) => {
            if error != 0 {
                errno::set(error);
            }
            -1
        }
    }
}

/// Wide characters into an array of known room.
struct WideString {
    /// The array.
    dst: *mut WChar,
    /// How many characters may be written.
    room: usize,
    /// How many have been, counting those that did not fit.
    used: usize,
}

impl WideTarget for WideString {
    fn put(&mut self, wcs: &[WChar]) {
        for &wc in wcs {
            if self.used < self.room {
                // SAFETY: `used` is below `room`, inside the caller's array.
                unsafe { self.dst.wrapping_add(self.used).write(wc) };
            }
            self.used += 1;
        }
    }

    fn failed(&self) -> bool {
        false
    }
}

/// Formats into the `n` wide characters at `s`, NUL-terminated. Returns the
/// count, or -1 when it and its NUL do not fit or formatting fails.
///
/// # Safety
///
/// `s` must be valid for writes of `n` wide characters, `fmt` a NUL-terminated
/// wide string, and `ap` a `va_list` whose arguments match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vswprintf(
    s: *mut WChar,
    n: usize,
    fmt: *const WChar,
    ap: *mut VaListTag,
) -> c_int {
    let Some(room) = n.checked_sub(1) else {
        return -1;
    };
    let mut target = WideString {
        dst: s,
        room,
        used: 0,
    };
    // SAFETY: the caller vouches for the array, the format and the arguments.
    let result = unsafe { format_wide(&mut target, fmt, ap) };
    // SAFETY: at most `room` characters were written, and `room` is inside.
    unsafe { s.wrapping_add(target.used.min(room)).write(0) };
    match result {
        Ok(count) if count > room => -1,
        result => finish(result),
    }
}

/// Wide characters written to a stream as multibyte sequences.
struct WideFile<'a> {
    /// The stream's bytes.
    sink: FileSink<'a>,
    /// Whether a character was no character in the locale.
    bad: bool,
}

impl WideTarget for WideFile<'_> {
    fn put(&mut self, wcs: &[WChar]) {
        let mut bytes = [0u8; 4];
        for &wc in wcs {
            let mut state = MbState::new();
            // SAFETY: `bytes` has room for the longest sequence.
            let len = unsafe { wcrtomb(bytes.as_mut_ptr().cast::<c_char>(), wc, &raw mut state) };
            if len == usize::MAX {
                self.bad = true;
                return;
            }
            self.sink.write(bytes.get(..len).unwrap_or_default());
        }
    }

    fn failed(&self) -> bool {
        self.bad || self.sink.failed()
    }
}

/// `vfwprintf` on a stream whose lock is held.
///
/// # Safety
///
/// As [`vfwprintf`].
unsafe fn vfwprintf_locked(inner: &mut Inner, fmt: *const WChar, ap: *mut VaListTag) -> c_int {
    if inner.orientation == 0 {
        inner.orientation = 1;
    }
    let old_error = inner.error;
    inner.error = false;
    let (result, bad) = {
        let mut target = WideFile {
            sink: FileSink::new(inner),
            bad: false,
        };
        // SAFETY: the caller vouches for the format and arguments.
        let result = unsafe { format_wide(&mut target, fmt, ap) };
        target.sink.flush();
        (result, target.bad)
    };
    if bad {
        inner.fail(errno::EILSEQ);
    }
    let failed = inner.error;
    inner.error |= old_error;
    match result {
        Ok(_) if failed => -1,
        result => finish(result),
    }
}

/// Formats to `stream`.
///
/// # Safety
///
/// `stream` must be a live stream, `fmt` a NUL-terminated wide string, and
/// `ap` a `va_list` whose arguments match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vfwprintf(
    stream: *mut File,
    fmt: *const WChar,
    ap: *mut VaListTag,
) -> c_int {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a live stream, a format and its arguments.
        unsafe { vfwprintf_locked(inner, fmt, ap) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// Formats to standard output.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated wide string, and `ap` a `va_list` whose
/// arguments match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vwprintf(fmt: *const WChar, ap: *mut VaListTag) -> c_int {
    // SAFETY: standard output is a static stream, and the caller vouches for
    // the rest.
    unsafe { vfwprintf(file::stdout.load(Ordering::Relaxed), fmt, ap) }
}

va::variadic!(wprintf, 1, vwprintf);
va::variadic!(fwprintf, 2, vfwprintf);
va::variadic!(swprintf, 3, vswprintf);
