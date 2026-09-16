//! Wide-character streams: `fwide`, and reading and writing one wide character
//! or a wide string at a time, with glibc's `_unlocked` names beside them.
//!
//! A stream holds bytes. A wide character is written as its multibyte sequence
//! in the current locale, with `wcrtomb`, and read by feeding bytes to
//! `mbrtowc` until one completes, as musl 1.2.5's `stdio/fputwc.c` and
//! `fgetwc.c` (MIT) do:
//!
//! * A byte sequence that is no character sets the error indicator and `errno`
//!   to `EILSEQ` and returns `WEOF`. The byte that broke a sequence already
//!   begun is pushed back, so the next read starts with it.
//! * End of file part way through a sequence is the same error.
//! * `ungetwc` pushes back all of a character's bytes or none of them.
//! * The first wide read or write makes the stream wide-oriented, which
//!   `fwide` reports. The byte functions do not orient a stream, as in musl.
//!
//! The `wprintf` family is in [`super::wprintf`], and `open_wmemstream` in
//! [`super::memory`]. The `wscanf` family is not here yet.

use core::ffi::{c_char, c_int};
use core::ptr::null_mut;
use core::sync::atomic::Ordering;

use super::file::{self, File, Inner};
use crate::errno;
use crate::multibyte::{MbState, WChar, WEOF, WInt, mbrtowc, wcrtomb};

/// `(size_t)-1` from `mbrtowc` and `wcrtomb`: an invalid sequence.
const INVALID: usize = usize::MAX;
/// `(size_t)-2` from `mbrtowc`: a sequence not yet complete.
const INCOMPLETE: usize = usize::MAX - 1;
/// `MB_LEN_MAX`.
const MB_LEN_MAX: usize = 4;

/// Marks the stream wide-oriented unless it already has an orientation.
fn orient_wide(inner: &mut Inner) {
    if inner.orientation == 0 {
        inner.orientation = 1;
    }
}

/// The bytes of `wc` in the current locale, and how many there are, or `None`
/// if it is no character there.
fn encode(wc: WChar) -> Option<([u8; MB_LEN_MAX], usize)> {
    let mut bytes = [0u8; MB_LEN_MAX];
    let mut state = MbState::new();
    // SAFETY: `bytes` has room for the longest sequence, and `state` is live.
    let len = unsafe { wcrtomb(bytes.as_mut_ptr().cast::<c_char>(), wc, &raw mut state) };
    (len != INVALID).then_some((bytes, len))
}

/// Reads one wide character, as `fgetwc` does.
fn get_wide(inner: &mut Inner) -> WInt {
    orient_wide(inner);
    let mut state = MbState::new();
    let mut wc: WChar = 0;
    let mut first = true;
    loop {
        let Some(byte) = inner.get_byte() else {
            if !first {
                inner.fail(errno::EILSEQ);
            }
            return WEOF;
        };
        // SAFETY: `byte` is one readable byte, and `wc` and `state` are live.
        let ret = unsafe {
            mbrtowc(
                &raw mut wc,
                (&raw const byte).cast::<c_char>(),
                1,
                &raw mut state,
            )
        };
        match ret {
            INCOMPLETE => first = false,
            INVALID => {
                if !first {
                    let _ = inner.unget(byte);
                }
                inner.fail(errno::EILSEQ);
                return WEOF;
            }
            _ => return wc as WInt,
        }
    }
}

/// Writes one wide character, as `fputwc` does.
fn put_wide(inner: &mut Inner, wc: WChar) -> WInt {
    orient_wide(inner);
    let Some((bytes, len)) = encode(wc) else {
        inner.fail(errno::EILSEQ);
        return WEOF;
    };
    for &byte in bytes.get(..len).unwrap_or(&[]) {
        if !inner.put_byte(byte) {
            return WEOF;
        }
    }
    wc as WInt
}

/// Reads a line of at most `n - 1` wide characters into `s`, as `fgetws` does.
///
/// # Safety
///
/// `s` must be valid for writing `n` wide characters.
unsafe fn get_line(inner: &mut Inner, s: *mut WChar, n: c_int) -> *mut WChar {
    let Ok(n) = usize::try_from(n) else {
        return null_mut();
    };
    let Some(room) = n.checked_sub(1) else {
        return s;
    };
    let mut len = 0;
    while len < room {
        let wc = get_wide(inner);
        if wc == WEOF {
            break;
        }
        // SAFETY: `len` is below `n - 1`, inside the caller's buffer.
        unsafe { s.wrapping_add(len).write(wc as WChar) };
        len += 1;
        if wc == WInt::from(b'\n') {
            break;
        }
    }
    // SAFETY: `len` is at most `n - 1`.
    unsafe { s.wrapping_add(len).write(0) };
    if len == 0 || inner.error {
        null_mut()
    } else {
        s
    }
}

/// Writes the wide string `s`, as `fputws` does: 0, or -1 on failure.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string.
unsafe fn put_string(inner: &mut Inner, s: *const WChar) -> c_int {
    let mut at = s;
    loop {
        // SAFETY: the string has not ended before `at`.
        let wc = unsafe { at.read() };
        if wc == 0 {
            return 0;
        }
        if put_wide(inner, wc) == WEOF {
            return -1;
        }
        at = at.wrapping_add(1);
    }
}

/// Standard input's stream.
fn standard_input() -> *mut File {
    file::stdin.load(Ordering::Relaxed)
}

/// Standard output's stream.
fn standard_output() -> *mut File {
    file::stdout.load(Ordering::Relaxed)
}

/// Defines a function that runs `$body` on a stream's state under its lock, and
/// its `_unlocked` twin.
macro_rules! wide {
    (
        $(#[doc = $doc:literal])*
        fn $locked:ident / $unlocked:ident ($($arg:ident: $ty:ty),*) -> $ret:ty = $body:expr;
    ) => {
        $(#[doc = $doc])*
        ///
        /// # Safety
        ///
        /// `stream` must be a live stream, and pointer arguments valid as C
        /// requires.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $locked($($arg: $ty,)* stream: *mut File) -> $ret {
            // SAFETY: the caller passes a live stream and valid arguments.
            unsafe { file::locked(stream, |inner| $body(inner, $($arg),*)) }
        }

        /// The same, without taking the stream's lock.
        ///
        /// # Safety
        ///
        /// As the locked function, and the caller holds the lock or knows no
        /// other thread uses the stream.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $unlocked($($arg: $ty,)* stream: *mut File) -> $ret {
            // SAFETY: as above.
            unsafe { file::unlocked(stream, |inner| $body(inner, $($arg),*)) }
        }
    };
}

wide! {
    /// Reads the next wide character, or returns `WEOF`.
    fn fgetwc / fgetwc_unlocked() -> WInt = get_wide;
}

wide! {
    /// `fgetwc`.
    fn getwc / getwc_unlocked() -> WInt = get_wide;
}

wide! {
    /// Writes the wide character `wc`, returning it, or `WEOF`.
    fn fputwc / fputwc_unlocked(wc: WChar) -> WInt = put_wide;
}

wide! {
    /// `fputwc`.
    fn putwc / putwc_unlocked(wc: WChar) -> WInt = put_wide;
}

wide! {
    /// Reads at most `n - 1` wide characters into `s`, through a newline, and
    /// NUL-terminates them. Returns `s`, or null if nothing was read or an
    /// error occurred.
    fn fgetws / fgetws_unlocked(s: *mut WChar, n: c_int) -> *mut WChar =
        get_line;
}

wide! {
    /// Writes the wide string `s`. Returns 0, or -1 on failure.
    fn fputws / fputws_unlocked(s: *const WChar) -> c_int =
        put_string;
}

/// Reads the next wide character from standard input.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getwchar() -> WInt {
    // SAFETY: standard input is a live stream.
    unsafe { fgetwc(standard_input()) }
}

/// `getwchar`, without taking the stream's lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getwchar_unlocked() -> WInt {
    // SAFETY: as above; the lock is the caller's matter, as C says.
    unsafe { fgetwc_unlocked(standard_input()) }
}

/// Writes the wide character `wc` to standard output.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn putwchar(wc: WChar) -> WInt {
    // SAFETY: standard output is a live stream.
    unsafe { fputwc(wc, standard_output()) }
}

/// `putwchar`, without taking the stream's lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn putwchar_unlocked(wc: WChar) -> WInt {
    // SAFETY: as above.
    unsafe { fputwc_unlocked(wc, standard_output()) }
}

/// Pushes the wide character `wc` back onto the stream, as all of its bytes or
/// none. Returns `wc`, or `WEOF` if it is `WEOF`, no character, or does not
/// fit.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ungetwc(wc: WInt, stream: *mut File) -> WInt {
    if wc == WEOF {
        return WEOF;
    }
    let Some((bytes, len)) = encode(wc as WChar) else {
        return WEOF;
    };
    // SAFETY: the caller passes a live stream.
    unsafe {
        file::locked(stream, |inner| {
            orient_wide(inner);
            if inner.unget_bytes(bytes.get(..len).unwrap_or(&[])) {
                wc
            } else {
                WEOF
            }
        })
    }
}

/// Sets the stream's orientation if it has none: wide for a positive `mode`,
/// bytes for a negative one; 0 only asks. Returns the orientation: positive,
/// negative, or 0 for none.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fwide(stream: *mut File, mode: c_int) -> c_int {
    // SAFETY: the caller passes a live stream.
    unsafe {
        file::locked(stream, |inner| {
            if inner.orientation == 0 && mode != 0 {
                inner.orientation = if mode > 0 { 1 } else { -1 };
            }
            c_int::from(inner.orientation)
        })
    }
}
