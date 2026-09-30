//! `stdio_ext.h`, the Solaris and glibc functions that look inside a stream,
//! and glibc's `__uflow`, `__overflow`, `__getdelim` and `tmpfile64`.
//!
//! `__uflow` and `__overflow` are what a program built against glibc's
//! headers calls from its inline `getc_unlocked` and `putc_unlocked` once the
//! stream's own read or write pointer has met its end. Here those pointers are
//! always null (`file.rs`, `GlibcHead`), so every such byte comes through
//! them: GLib's `g_io_channel` readers call `__uflow` for each one.
//!
//! As musl 1.2.5's `src/stdio/ext.c` and `ext2.c` (MIT; see [`crate::math`]
//! for the notice), but for `__fsetlocking`, which answers
//! `FSETLOCKING_INTERNAL` as glibc does: every stream here locks itself.

use core::ffi::{c_char, c_int};

use super::EOF;
use super::file::{self, File, Inner};
use super::io;

/// `FSETLOCKING_INTERNAL`: the library locks the stream around each call.
const FSETLOCKING_INTERNAL: c_int = 1;

/// Defines a query over a stream's state, taken with its lock.
macro_rules! query {
    ($(#[$doc:meta])* fn $name:ident -> $ret:ty = $body:expr;) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// `stream` must be a live stream.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name(stream: *mut File) -> $ret {
            // SAFETY: the caller passes a live stream.
            unsafe { file::locked(stream, $body) }
        }
    };
}

query! {
    /// The bytes of output in the buffer, not yet written.
    fn __fpending -> usize = |inner: &mut Inner| inner.pending_output();
}

query! {
    /// The bytes read ahead of the program, in the buffer and pushback.
    fn __freadahead -> usize = |inner: &mut Inner| inner.read_ahead();
}

query! {
    /// The buffer's size.
    fn __fbufsize -> usize = |inner: &mut Inner| inner.buffer_size();
}

query! {
    /// Whether the stream is read-only or was last read.
    fn __freading -> c_int = |inner: &mut Inner| c_int::from(!inner.writable || inner.is_reading());
}

query! {
    /// Whether the stream is write-only or was last written.
    fn __fwriting -> c_int = |inner: &mut Inner| c_int::from(!inner.readable || inner.is_writing());
}

query! {
    /// Whether the stream may be read.
    fn __freadable -> c_int = |inner: &mut Inner| c_int::from(inner.readable);
}

query! {
    /// Whether the stream may be written.
    fn __fwritable -> c_int = |inner: &mut Inner| c_int::from(inner.writable);
}

query! {
    /// Whether the stream is line-buffered.
    fn __flbf -> c_int = |inner: &mut Inner| c_int::from(inner.is_line_buffered());
}

/// Discards the stream's buffered input and output.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fpurge(stream: *mut File) -> c_int {
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, Inner::purge) };
    0
}

/// Sets the stream's error indicator.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fseterr(stream: *mut File) {
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, |inner: &mut Inner| inner.error = true) };
}

/// Asks the stream to lock itself or leave locking to the caller, and
/// returns how it locked before. Every stream here locks itself, and a
/// program that asks for `FSETLOCKING_BYCALLER` only loses the cost it hoped
/// to save.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __fsetlocking(_stream: *mut File, _how: c_int) -> c_int {
    FSETLOCKING_INTERNAL
}

/// Flushes every line-buffered stream: every stream, here, as musl does.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn _flushlbf() {
    let _ = file::flush_all();
}

/// The next byte of the stream, consumed, or `EOF`: glibc's slow path of
/// `getc_unlocked`.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __uflow(stream: *mut File) -> c_int {
    // SAFETY: the caller passes a live stream, and glibc's macro calls this
    // without the lock, as `getc_unlocked` is.
    unsafe { io::fgetc_unlocked(stream) }
}

/// Writes `c` to the stream, or with `EOF` only flushes it: glibc's slow
/// path of `putc_unlocked`. Returns `c` as a byte, 0 for a flush, or `EOF`.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __overflow(stream: *mut File, c: c_int) -> c_int {
    if c == EOF {
        // SAFETY: the caller passes a live stream.
        return unsafe { io::fflush_unlocked(stream) };
    }
    // SAFETY: as above.
    unsafe { io::fputc_unlocked(c, stream) }
}

/// glibc's internal name for [`io::getdelim`], which its headers call.
///
/// # Safety
///
/// As `getdelim`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __getdelim(
    lineptr: *mut *mut c_char,
    n: *mut usize,
    delimiter: c_int,
    stream: *mut File,
) -> isize {
    // SAFETY: the caller's contract is `getdelim`'s.
    unsafe { io::getdelim(lineptr, n, delimiter, stream) }
}

/// glibc's large-file name for `tmpfile`: every file here is large.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tmpfile64() -> *mut File {
    super::open::tmpfile()
}
