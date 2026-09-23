//! Formatted wide input: `fwscanf`, `swscanf`, `wscanf` and their `v` forms,
//! with glibc's `__isoc99_` and `__isoc23_` names for each.
//!
//! These run [`super::scanf`]'s engine, as musl 1.2.5's `stdio/vfwscanf.c`
//! (MIT) follows its narrow `vfscanf`, over a wide source that gives one unit
//! per wide character: widths and `%n` count characters, and numbers read as
//! they do from bytes. The wide format goes to the engine as a narrowed copy
//! with the same indices, and the engine compares a character the copy cannot
//! hold, and decides `%[` sets, from the wide format itself. White space is
//! `iswspace`'s. `%ls`, `%lc` and `%l[` store wide characters; `%s`, `%c` and
//! `%[` store each character's multibyte sequence.
//!
//! `fwscanf` reads a stream as `fgetwc` does, making it wide-oriented, and gives
//! back the whole of a character that ends a field.

use core::ffi::{c_char, c_int};
use core::sync::atomic::Ordering;

use super::EOF;
use super::file::{self, File, Inner, stdin};
use super::scanf::{Source, scan};
use super::wide::get_wide;
use crate::multibyte::{MbState, WChar, WEOF, WInt, wcrtomb};
use crate::va::{self, VaListArg};
use crate::wchar::wcslen;
use crate::wctype::iswspace;
use crate::{errno, malloc};

/// The unit the engine reads for `wc`: the character when it is ASCII, a space
/// for other white space, and `0x80` for anything else.
fn unit(wc: WChar) -> u8 {
    match u8::try_from(wc) {
        Ok(byte) if byte.is_ascii() => byte,
        _ if iswspace(wc as WInt) != 0 => b' ',
        _ => 0x80,
    }
}

/// A wide string, for `swscanf`.
#[derive(Debug)]
struct WideText {
    /// The string.
    start: *const WChar,
    /// How many characters have been read. None of them is the NUL.
    pos: usize,
}

impl WideText {
    /// The character at `i`.
    fn at(&self, i: usize) -> WChar {
        // SAFETY: `vswscanf` made this from a NUL-terminated wide string, and
        // every index asked for is at or before its NUL.
        unsafe { self.start.wrapping_add(i).read() }
    }
}

impl Source for WideText {
    fn next(&mut self) -> Option<u8> {
        let wc = self.at(self.pos);
        if wc == 0 {
            return None;
        }
        self.pos += 1;
        Some(unit(wc))
    }

    fn back(&mut self, _byte: u8) {
        self.pos = self.pos.saturating_sub(1);
    }

    fn last_wide(&self) -> Option<WChar> {
        Some(self.pos.checked_sub(1).map_or(0, |i| self.at(i)))
    }
}

/// A stream whose lock is held, for `fwscanf`.
#[derive(Debug)]
struct WideStream<'a> {
    /// The stream's state.
    inner: &'a mut Inner,
    /// The character the last unit stood for.
    last: WChar,
}

impl Source for WideStream<'_> {
    fn next(&mut self) -> Option<u8> {
        let wc = get_wide(self.inner);
        if wc == WEOF {
            return None;
        }
        self.last = wc as WChar;
        Some(unit(self.last))
    }

    fn back(&mut self, _byte: u8) {
        let mut bytes = [0u8; 4];
        let mut state = MbState::new();
        // SAFETY: `bytes` has room for the longest sequence.
        let len = unsafe { wcrtomb(bytes.as_mut_ptr().cast(), self.last, &raw mut state) };
        if len != usize::MAX {
            let _ = self.inner.unget_bytes(bytes.get(..len).unwrap_or_default());
        }
    }

    fn last_wide(&self) -> Option<WChar> {
        Some(self.last)
    }
}

/// Runs `op` on a narrowed copy of the wide format `fmt`, with the same
/// indices, and returns what it returns, or `EOF` if there is no memory.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated wide string.
unsafe fn with_narrow_format(fmt: *const WChar, op: impl FnOnce(*const c_char) -> c_int) -> c_int {
    // SAFETY: the caller passes a NUL-terminated wide string.
    let len = unsafe { wcslen(fmt) };
    let narrow = malloc::malloc(len + 1).cast::<u8>();
    if narrow.is_null() {
        errno::set(errno::ENOMEM);
        return EOF;
    }
    for i in 0..=len {
        // SAFETY: `i` is at most `len`, inside the format and its NUL.
        let wc = unsafe { fmt.wrapping_add(i).read() };
        let byte = if wc == 0 { 0 } else { unit(wc) };
        // SAFETY: `narrow` has room for `len + 1` bytes.
        unsafe { narrow.wrapping_add(i).write(byte) };
    }
    let result = op(narrow.cast());
    // SAFETY: `narrow` came from `malloc` and is not used again.
    unsafe { malloc::free(narrow.cast()) };
    result
}

/// Reads the wide string `ws` by the wide format `fmt`, storing through the
/// pointers in `ap`.
///
/// # Safety
///
/// `ws` and `fmt` must be NUL-terminated wide strings, and `ap` a `va_list`
/// holding a pointer of the right type for each conversion.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vswscanf(ws: *const WChar, fmt: *const WChar, ap: VaListArg) -> c_int {
    let op = |narrow| {
        let mut source = WideText { start: ws, pos: 0 };
        // SAFETY: the caller passes a string, a format and its arguments, and
        // `narrow` is the format's narrowed copy.
        unsafe { scan(&mut source, narrow, fmt, ap) }
    };
    // SAFETY: the caller passes a NUL-terminated wide format.
    unsafe { with_narrow_format(fmt, op) }
}

/// Reads `stream` by the wide format `fmt`, storing through the pointers in
/// `ap`.
///
/// # Safety
///
/// `stream` must be a live stream, `fmt` a NUL-terminated wide string, and `ap`
/// a `va_list` holding a pointer of the right type for each conversion.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vfwscanf(stream: *mut File, fmt: *const WChar, ap: VaListArg) -> c_int {
    let op = |narrow| {
        let read = |inner: &mut Inner| {
            let mut source = WideStream { inner, last: 0 };
            // SAFETY: as in `vswscanf`, over a live stream.
            unsafe { scan(&mut source, narrow, fmt, ap) }
        };
        // SAFETY: the caller passes a live stream.
        unsafe { file::locked(stream, read) }
    };
    // SAFETY: the caller passes a NUL-terminated wide format.
    unsafe { with_narrow_format(fmt, op) }
}

/// Reads standard input by the wide format `fmt`.
///
/// # Safety
///
/// As [`vfwscanf`], without the stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vwscanf(fmt: *const WChar, ap: VaListArg) -> c_int {
    // SAFETY: `stdin` is a live stream, and the caller passes the rest.
    unsafe { vfwscanf(stdin.load(Ordering::Relaxed), fmt, ap) }
}

/// Defines glibc's other names for a `v` function.
macro_rules! alias {
    ($name:ident = $target:ident ($($arg:ident: $ty:ty),*)) => {
        #[doc = concat!("`", stringify!($target), "`, under glibc's name.")]
        ///
        /// # Safety
        ///
        #[doc = concat!("As [`", stringify!($target), "`].")]
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name($($arg: $ty),*) -> c_int {
            // SAFETY: the caller's promises are the target's.
            unsafe { $target($($arg),*) }
        }
    };
}

alias!(__isoc99_vswscanf = vswscanf(ws: *const WChar, fmt: *const WChar, ap: VaListArg));
alias!(__isoc99_vfwscanf = vfwscanf(stream: *mut File, fmt: *const WChar, ap: VaListArg));
alias!(__isoc99_vwscanf = vwscanf(fmt: *const WChar, ap: VaListArg));
alias!(__isoc23_vswscanf = vswscanf(ws: *const WChar, fmt: *const WChar, ap: VaListArg));
alias!(__isoc23_vfwscanf = vfwscanf(stream: *mut File, fmt: *const WChar, ap: VaListArg));
alias!(__isoc23_vwscanf = vwscanf(fmt: *const WChar, ap: VaListArg));

va::variadic!(swscanf, 2, vswscanf);
va::variadic!(fwscanf, 2, vfwscanf);
va::variadic!(wscanf, 1, vwscanf);
va::variadic!(__isoc99_swscanf, 2, vswscanf);
va::variadic!(__isoc99_fwscanf, 2, vfwscanf);
va::variadic!(__isoc99_wscanf, 1, vwscanf);
va::variadic!(__isoc23_swscanf, 2, vswscanf);
va::variadic!(__isoc23_fwscanf, 2, vfwscanf);
va::variadic!(__isoc23_wscanf, 1, vwscanf);
