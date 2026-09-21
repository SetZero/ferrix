//! Reading, writing and positioning streams, their indicators, buffering and
//! locks, and `perror`.
//!
//! Each function that takes a stream's lock has an `_unlocked` twin wherever
//! musl's header declares one; both run the same body on the stream's state.

use core::ffi::{CStr, c_char, c_int, c_long, c_void};
use core::mem::size_of;
use core::ptr::null_mut;
use core::sync::atomic::Ordering;

use super::EOF;
use super::file::{self, File, Inner, Mode, SEEK_SET};
use super::printf::{FileSink, Sink};
use crate::{errno, malloc, strerror, string};

/// `_IOFBF`.
const IOFBF: c_int = 0;
/// `_IOLBF`.
const IOLBF: c_int = 1;
/// `_IONBF`.
const IONBF: c_int = 2;
/// `BUFSIZ`, the size `setbuf` assumes.
const BUFSIZ: usize = 1024;

/// C's `fpos_t`: in musl's header a 16-byte union whose `long long` member
/// holds the offset here.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Fpos {
    /// The offset.
    offset: i64,
    /// The rest of the union.
    rest: [u8; 8],
}

const _: () = assert!(size_of::<Fpos>() == 16);

/// Standard input's stream.
fn standard_input() -> *mut File {
    file::stdin.load(Ordering::Relaxed)
}

/// Standard output's stream.
fn standard_output() -> *mut File {
    file::stdout.load(Ordering::Relaxed)
}

/// Defines a locked function and its `_unlocked` twin over one body taking the
/// stream's state.
macro_rules! both {
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

/// The end-of-file indicator.
fn eof_of(inner: &mut Inner) -> c_int {
    c_int::from(inner.eof)
}

/// The error indicator.
fn error_of(inner: &mut Inner) -> c_int {
    c_int::from(inner.error)
}

/// Clears both indicators.
fn clear(inner: &mut Inner) {
    inner.eof = false;
    inner.error = false;
}

both! {
    /// Whether the stream's end-of-file indicator is set.
    fn feof / feof_unlocked() -> c_int = eof_of;
}

both! {
    /// Whether the stream's error indicator is set.
    fn ferror / ferror_unlocked() -> c_int = error_of;
}

both! {
    /// Clears the stream's end-of-file and error indicators.
    fn clearerr / clearerr_unlocked() -> () = clear;
}

/// Reads a byte as `fgetc` returns it.
fn get(inner: &mut Inner) -> c_int {
    inner.get_byte().map_or(EOF, c_int::from)
}

/// Writes a byte as `fputc` returns it.
fn put(inner: &mut Inner, c: c_int) -> c_int {
    let byte = c as u8;
    if inner.put_byte(byte) {
        c_int::from(byte)
    } else {
        EOF
    }
}

both! {
    /// Reads the next byte, or returns `EOF`.
    fn fgetc / fgetc_unlocked() -> c_int = get;
}

both! {
    /// `fgetc`.
    fn getc / getc_unlocked() -> c_int = get;
}

both! {
    /// Writes the byte `c`, returning it, or `EOF`.
    fn fputc / fputc_unlocked(c: c_int) -> c_int = put;
}

both! {
    /// `fputc`.
    fn putc / putc_unlocked(c: c_int) -> c_int = put;
}

/// Reads the next byte from standard input.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getchar() -> c_int {
    // SAFETY: standard input is a static stream.
    unsafe { fgetc(standard_input()) }
}

/// `getchar` without the lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getchar_unlocked() -> c_int {
    // SAFETY: standard input is a static stream; C leaves its locking to the
    // caller.
    unsafe { fgetc_unlocked(standard_input()) }
}

/// Writes the byte `c` to standard output.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn putchar(c: c_int) -> c_int {
    // SAFETY: standard output is a static stream.
    unsafe { fputc(c, standard_output()) }
}

/// `putchar` without the lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn putchar_unlocked(c: c_int) -> c_int {
    // SAFETY: standard output is a static stream; C leaves its locking to the
    // caller.
    unsafe { fputc_unlocked(c, standard_output()) }
}

/// The body of `fread`.
///
/// # Safety
///
/// `ptr` must be valid for writes of `size * count` bytes.
unsafe fn read_items(inner: &mut Inner, ptr: *mut c_void, size: usize, count: usize) -> usize {
    let Some(total) = size.checked_mul(count) else {
        inner.fail(errno::EOVERFLOW);
        return 0;
    };
    if total == 0 {
        return 0;
    }
    // SAFETY: the caller vouches for the destination.
    let done = unsafe { inner.read(ptr.cast(), total) };
    done / size
}

/// The body of `fwrite`.
///
/// # Safety
///
/// `ptr` must be valid for reads of `size * count` bytes.
unsafe fn write_items(inner: &mut Inner, ptr: *const c_void, size: usize, count: usize) -> usize {
    let Some(total) = size.checked_mul(count) else {
        inner.fail(errno::EOVERFLOW);
        return 0;
    };
    if total == 0 {
        return 0;
    }
    // SAFETY: the caller vouches for the source.
    let done = unsafe { inner.write(ptr.cast(), total) };
    done / size
}

/// Reads up to `count` items of `size` bytes into `ptr`, returning how many
/// whole items were read.
///
/// # Safety
///
/// `ptr` must be valid for writes of `size * count` bytes, and `stream` a live
/// stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fread(
    ptr: *mut c_void,
    size: usize,
    count: usize,
    stream: *mut File,
) -> usize {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a live stream and a valid destination.
        unsafe { read_items(inner, ptr, size, count) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// `fread` without the lock.
///
/// # Safety
///
/// As `fread`, and the caller holds the lock or need not.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fread_unlocked(
    ptr: *mut c_void,
    size: usize,
    count: usize,
    stream: *mut File,
) -> usize {
    let op = |inner: &mut Inner| {
        // SAFETY: as above.
        unsafe { read_items(inner, ptr, size, count) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::unlocked(stream, op) }
}

/// Writes `count` items of `size` bytes from `ptr`, returning how many whole
/// items were written.
///
/// # Safety
///
/// `ptr` must be valid for reads of `size * count` bytes, and `stream` a live
/// stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fwrite(
    ptr: *const c_void,
    size: usize,
    count: usize,
    stream: *mut File,
) -> usize {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a live stream and a valid source.
        unsafe { write_items(inner, ptr, size, count) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// `fwrite` without the lock.
///
/// # Safety
///
/// As `fwrite`, and the caller holds the lock or need not.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fwrite_unlocked(
    ptr: *const c_void,
    size: usize,
    count: usize,
    stream: *mut File,
) -> usize {
    let op = |inner: &mut Inner| {
        // SAFETY: as above.
        unsafe { write_items(inner, ptr, size, count) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::unlocked(stream, op) }
}

/// The body of `fgets`.
///
/// # Safety
///
/// `s` must be valid for writes of `n` bytes.
unsafe fn read_line(inner: &mut Inner, s: *mut c_char, n: c_int) -> *mut c_char {
    let Some(limit) = usize::try_from(n).ok().and_then(|n| n.checked_sub(1)) else {
        errno::set(errno::EINVAL);
        return null_mut();
    };
    let had_error = inner.error;
    let mut done = 0;
    while done < limit {
        let Some((at, available)) = inner.peek() else {
            break;
        };
        let take = available.min(limit - done);
        // SAFETY: `peek` returned `available` readable bytes.
        let newline = unsafe { string::memchr(at.cast(), c_int::from(b'\n'), take) };
        let count = if newline.is_null() {
            take
        } else {
            newline.addr() - at.addr() + 1
        };
        // SAFETY: `count` is within both what `peek` returned and the room
        // left in `s`.
        let _ = unsafe { string::memcpy(s.wrapping_add(done).cast(), at.cast(), count) };
        inner.consume(count);
        done += count;
        if !newline.is_null() {
            break;
        }
    }
    if (done == 0 && limit != 0) || (inner.error && !had_error) {
        return null_mut();
    }
    // SAFETY: `done` is at most `n - 1`.
    unsafe { s.wrapping_add(done).write(0) };
    s
}

/// Reads a line of at most `n - 1` bytes into `s`, keeping its newline. Null
/// at the end of the file with nothing read, leaving `s` unchanged, or on an
/// error.
///
/// # Safety
///
/// `s` must be valid for writes of `n` bytes, and `stream` a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fgets(s: *mut c_char, n: c_int, stream: *mut File) -> *mut c_char {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a live stream and a valid buffer.
        unsafe { read_line(inner, s, n) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// `fgets` without the lock.
///
/// # Safety
///
/// As `fgets`, and the caller holds the lock or need not.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fgets_unlocked(
    s: *mut c_char,
    n: c_int,
    stream: *mut File,
) -> *mut c_char {
    let op = |inner: &mut Inner| {
        // SAFETY: as above.
        unsafe { read_line(inner, s, n) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::unlocked(stream, op) }
}

/// The body of `fputs`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
unsafe fn write_string(inner: &mut Inner, s: *const c_char) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { string::strlen(s) };
    // SAFETY: `strlen` found `len` readable bytes.
    if unsafe { inner.write(s.cast(), len) } == len {
        0
    } else {
        EOF
    }
}

/// Writes the string `s`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string, and `stream` a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fputs(s: *const c_char, stream: *mut File) -> c_int {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a live stream and a string.
        unsafe { write_string(inner, s) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// `fputs` without the lock.
///
/// # Safety
///
/// As `fputs`, and the caller holds the lock or need not.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fputs_unlocked(s: *const c_char, stream: *mut File) -> c_int {
    let op = |inner: &mut Inner| {
        // SAFETY: as above.
        unsafe { write_string(inner, s) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::unlocked(stream, op) }
}

/// Writes `s` and a newline to standard output.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn puts(s: *const c_char) -> c_int {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a string.
        if unsafe { write_string(inner, s) } == 0 && inner.put_byte(b'\n') {
            0
        } else {
            EOF
        }
    };
    // SAFETY: standard output is a static stream.
    unsafe { file::locked(standard_output(), op) }
}

/// The body of `getdelim`.
///
/// # Safety
///
/// As `getdelim`.
unsafe fn read_delimited(
    inner: &mut Inner,
    lineptr: *mut *mut c_char,
    n: *mut usize,
    delimiter: c_int,
) -> isize {
    if lineptr.is_null() || n.is_null() {
        inner.fail(errno::EINVAL);
        return -1;
    }
    // SAFETY: the caller passes valid pointers.
    let mut buf = unsafe { lineptr.read() };
    let mut cap = if buf.is_null() {
        0
    } else {
        // SAFETY: as above.
        unsafe { n.read() }
    };
    let had_error = inner.error;
    let mut len = 0_usize;
    while let Some((at, available)) = inner.peek() {
        // SAFETY: `peek` returned `available` readable bytes.
        let found = unsafe { string::memchr(at.cast(), delimiter, available) };
        let count = if found.is_null() {
            available
        } else {
            found.addr() - at.addr() + 1
        };
        let Some(need) = len.checked_add(count).and_then(|need| need.checked_add(1)) else {
            inner.fail(errno::EOVERFLOW);
            return -1;
        };
        if need > cap {
            let wanted = need.max(cap.saturating_mul(2));
            // SAFETY: the buffer is null or came from `malloc`, as C requires.
            let grown = unsafe { malloc::realloc(buf.cast(), wanted) }.cast::<c_char>();
            if grown.is_null() {
                inner.fail(errno::ENOMEM);
                return -1;
            }
            buf = grown;
            cap = wanted;
            // SAFETY: the caller passes valid pointers.
            unsafe { lineptr.write(buf) };
            // SAFETY: as above.
            unsafe { n.write(cap) };
        }
        // SAFETY: the buffer has room for `count` more bytes and a NUL, and
        // `peek` returned `count` readable ones.
        let _ = unsafe { string::memcpy(buf.wrapping_add(len).cast(), at.cast(), count) };
        inner.consume(count);
        len += count;
        if !found.is_null() {
            break;
        }
    }
    if len == 0 || (inner.error && !had_error) {
        return -1;
    }
    // SAFETY: the buffer has room for the NUL.
    unsafe { buf.wrapping_add(len).write(0) };
    len as isize
}

/// Reads up to and including the byte `delimiter` into `*lineptr`, growing it
/// with `realloc` and updating `*n`. Returns the length read, or -1 at the end
/// of the file with nothing read or on an error.
///
/// # Safety
///
/// `lineptr` and `n` must be valid, `*lineptr` null or from `malloc` with `*n`
/// bytes, and `stream` a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getdelim(
    lineptr: *mut *mut c_char,
    n: *mut usize,
    delimiter: c_int,
    stream: *mut File,
) -> isize {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a live stream and valid pointers.
        unsafe { read_delimited(inner, lineptr, n, delimiter) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// `getdelim` with a newline.
///
/// # Safety
///
/// As `getdelim`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getline(
    lineptr: *mut *mut c_char,
    n: *mut usize,
    stream: *mut File,
) -> isize {
    // SAFETY: as above.
    unsafe { getdelim(lineptr, n, c_int::from(b'\n'), stream) }
}

/// Pushes the byte `c` back onto the stream. Up to 8 bytes can be pushed back;
/// a seek discards them. Returns `c` as a byte, or `EOF`.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ungetc(c: c_int, stream: *mut File) -> c_int {
    if c == EOF {
        return EOF;
    }
    // SAFETY: the caller passes a live stream.
    unsafe {
        file::locked(stream, |inner| {
            if inner.unget(c as u8) {
                c_int::from(c as u8)
            } else {
                EOF
            }
        })
    }
}

/// Reads a word of `sizeof(int)` bytes.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getw(stream: *mut File) -> c_int {
    let mut word: c_int = 0;
    // SAFETY: `word` is a live local of the size read.
    let read = unsafe { fread((&raw mut word).cast(), size_of::<c_int>(), 1, stream) };
    if read == 1 { word } else { EOF }
}

/// Writes `word` as `sizeof(int)` bytes.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn putw(word: c_int, stream: *mut File) -> c_int {
    // SAFETY: `word` is a live local of the size written.
    let written = unsafe { fwrite((&raw const word).cast(), size_of::<c_int>(), 1, stream) };
    if written == 1 { 0 } else { EOF }
}

/// The body of `fflush` on one stream.
fn flush_one(inner: &mut Inner) -> c_int {
    if inner.flush() { 0 } else { EOF }
}

/// Writes the stream's pending output, or with a null stream every stream's.
/// On an input stream on a seekable file, gives unread input back to the file.
///
/// # Safety
///
/// `stream` must be null or a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fflush(stream: *mut File) -> c_int {
    if stream.is_null() {
        return if file::flush_all() { 0 } else { EOF };
    }
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, flush_one) }
}

/// `fflush` without the lock.
///
/// # Safety
///
/// As `fflush`, and the caller holds the lock or need not.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fflush_unlocked(stream: *mut File) -> c_int {
    if stream.is_null() {
        return if file::flush_all() { 0 } else { EOF };
    }
    // SAFETY: as above.
    unsafe { file::unlocked(stream, flush_one) }
}

/// Moves the stream's position, as `fseeko`.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fseek(stream: *mut File, offset: c_long, whence: c_int) -> c_int {
    // SAFETY: the caller passes a live stream.
    unsafe { fseeko(stream, offset, whence) }
}

/// Moves the stream's position to `offset` from the start, the current
/// position or the end. Flushes pending output first, and discards buffered
/// input and pushback after. Returns 0, or -1 with `errno` set.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fseeko(stream: *mut File, offset: i64, whence: c_int) -> c_int {
    // SAFETY: the caller passes a live stream.
    let moved = unsafe { file::locked(stream, |inner| inner.seek(offset, whence)) };
    if moved { 0 } else { -1 }
}

/// The stream's position, as `ftello`.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ftell(stream: *mut File) -> c_long {
    // SAFETY: the caller passes a live stream.
    unsafe { ftello(stream) }
}

/// The stream's position, counting buffered input and output, or -1 with
/// `errno` set. In append mode with output pending, that is the end of the
/// file plus the output.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ftello(stream: *mut File) -> i64 {
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, Inner::tell) }
}

/// glibc's large-file name for [`fseeko`]; `off_t` is already 64-bit here.
///
/// # Safety
///
/// As [`fseeko`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fseeko64(stream: *mut File, offset: i64, whence: c_int) -> c_int {
    // SAFETY: the caller's contract is `fseeko`'s.
    unsafe { fseeko(stream, offset, whence) }
}

/// glibc's large-file name for [`ftello`].
///
/// # Safety
///
/// As [`ftello`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ftello64(stream: *mut File) -> i64 {
    // SAFETY: the caller's contract is `ftello`'s.
    unsafe { ftello(stream) }
}

/// Moves to the start of the stream and clears its error indicator.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn rewind(stream: *mut File) {
    // SAFETY: the caller passes a live stream.
    unsafe {
        file::locked(stream, |inner| {
            let _ = inner.seek(0, SEEK_SET);
            inner.error = false;
        });
    }
}

/// Stores the stream's position in `*pos`.
///
/// # Safety
///
/// `stream` must be a live stream, and `pos` valid for writes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fgetpos(stream: *mut File, pos: *mut Fpos) -> c_int {
    // SAFETY: the caller passes a live stream.
    let offset = unsafe { ftello(stream) };
    if offset < 0 {
        return -1;
    }
    // SAFETY: the caller passes a valid `fpos_t`.
    unsafe {
        pos.write(Fpos {
            offset,
            rest: [0; 8],
        });
    }
    0
}

/// Moves the stream to a position `fgetpos` stored.
///
/// # Safety
///
/// `stream` must be a live stream, and `pos` valid for reads.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fsetpos(stream: *mut File, pos: *const Fpos) -> c_int {
    // SAFETY: the caller passes a valid `fpos_t`.
    let offset = unsafe { pos.read() }.offset;
    // SAFETY: the caller passes a live stream.
    unsafe { fseeko(stream, offset, SEEK_SET) }
}

/// Chooses the stream's buffering: `_IOFBF`, `_IOLBF` or `_IONBF`, with the
/// `size` bytes at `buf`, or a buffer of `size` bytes allocated at the first
/// I/O if `buf` is null. Meant to be the first operation on the stream;
/// output already buffered is flushed first. Returns 0, or -1 with `EINVAL`
/// for an unknown mode.
///
/// # Safety
///
/// `stream` must be a live stream, and `buf` null or valid for `size` bytes
/// for as long as the stream uses it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setvbuf(
    stream: *mut File,
    buf: *mut c_char,
    mode: c_int,
    size: usize,
) -> c_int {
    let mode = match mode {
        IOFBF => Mode::Full,
        IOLBF => Mode::Line,
        IONBF => Mode::Unbuffered,
        _ => {
            errno::set(errno::EINVAL);
            return -1;
        }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, |inner| inner.set_buffer(buf.cast(), mode, size)) };
    0
}

/// Fully buffers the stream with the `BUFSIZ` bytes at `buf`, or unbuffers it
/// if `buf` is null.
///
/// # Safety
///
/// As `setvbuf`, with `BUFSIZ` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setbuf(stream: *mut File, buf: *mut c_char) {
    // SAFETY: as above.
    unsafe { setbuffer(stream, buf, BUFSIZ) };
}

/// Fully buffers the stream with the `size` bytes at `buf`, or unbuffers it if
/// `buf` is null.
///
/// # Safety
///
/// As `setvbuf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setbuffer(stream: *mut File, buf: *mut c_char, size: usize) {
    let mode = if buf.is_null() { IONBF } else { IOFBF };
    // SAFETY: the caller vouches for the stream and buffer.
    let _ = unsafe { setvbuf(stream, buf, mode, size) };
}

/// Line-buffers the stream.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setlinebuf(stream: *mut File) {
    // SAFETY: the caller passes a live stream.
    let _ = unsafe { setvbuf(stream, null_mut(), IOLBF, 0) };
}

/// Takes the stream's lock, waiting for another thread to release it.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn flockfile(stream: *mut File) {
    // SAFETY: the caller passes a live stream.
    unsafe { &*stream }.lock().lock();
}

/// Takes the stream's lock if no other thread holds it. Returns 0 if it did.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ftrylockfile(stream: *mut File) -> c_int {
    // SAFETY: the caller passes a live stream.
    if unsafe { &*stream }.lock().try_lock() {
        0
    } else {
        -1
    }
}

/// Releases the stream's lock once.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn funlockfile(stream: *mut File) {
    // SAFETY: the caller passes a live stream.
    unsafe { &*stream }.lock().unlock();
}

/// Writes `msg: `, the text of `errno`, and a newline to standard error, in
/// one write when they fit. A null or empty `msg` writes only the text.
/// `errno` is left as it was.
///
/// # Safety
///
/// `msg` must be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn perror(msg: *const c_char) {
    // SAFETY: the pointer is this thread's `errno`.
    let saved = unsafe { errno::__errno_location().read() };
    // SAFETY: `strerror` returns a static string.
    let text = unsafe { CStr::from_ptr(strerror::strerror(saved)) };
    let prefix = if msg.is_null() {
        &[][..]
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        unsafe { CStr::from_ptr(msg) }.to_bytes()
    };
    let err = file::stderr.load(Ordering::Relaxed);
    // SAFETY: standard error is a static stream.
    unsafe {
        file::locked(err, |inner| {
            let mut sink = FileSink::new(inner);
            if !prefix.is_empty() {
                sink.write(prefix);
                sink.write(b": ");
            }
            sink.write(text.to_bytes());
            sink.write(b"\n");
            sink.flush();
        });
    }
    errno::set(saved);
}
