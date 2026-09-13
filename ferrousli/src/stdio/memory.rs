//! Streams over memory and over a program's functions: `open_memstream`,
//! `fmemopen` and `fopencookie`.
//!
//! The semantics are musl's (MIT), from `src/stdio/open_memstream.c`,
//! `fmemopen.c` and `fopencookie.c` in musl 1.2.5, rewritten over this
//! library's backends:
//!
//! * `open_memstream` grows a `malloc` buffer as it is written, keeps it
//!   NUL-terminated, and stores its address and the position after the last
//!   write through the pointers it was given. It can seek, including past the
//!   end, which leaves zeros.
//! * `fmemopen` reads and writes a fixed buffer. Mode `r` sees the whole
//!   buffer; `a` starts at the first NUL and always writes at the end; `w+`
//!   empties the buffer. A write that extends the contents NUL-terminates
//!   them if there is room. A write past the end fails with `ENOSPC`. With a
//!   null buffer, one is allocated and freed at close.
//! * `fopencookie` calls the program's functions. A missing read function
//!   makes reads fail, a missing write function discards output, a missing
//!   seek function makes seeks fail with `ENOTSUP`, and a missing close
//!   function succeeds.
//!
//! Memory streams are unbuffered, so what `open_memstream` and `fmemopen`
//! report is always current, not only after `fflush`. Cookie streams are
//! fully buffered, as glibc makes them.

use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;

use super::file::{self, Backend, File, Inner, Mode, SEEK_CUR, SEEK_END, SEEK_SET};
use super::open::parse_mode;
use crate::{errno, malloc, string};

/// The base a `whence` measures from, or `EINVAL`.
fn base(whence: c_int, position: usize, length: usize) -> Result<usize, c_int> {
    match whence {
        SEEK_SET => Ok(0),
        SEEK_CUR => Ok(position),
        SEEK_END => Ok(length),
        _ => Err(errno::EINVAL),
    }
}

/// `base + offset`, if it lies within `0..=limit`.
fn offset_within(base: usize, offset: i64, limit: usize) -> Result<usize, c_int> {
    let target = (base as i128) + i128::from(offset);
    if target < 0 || target > limit as i128 {
        return Err(errno::EINVAL);
    }
    Ok(target as usize)
}

/// The state of a stream from `open_memstream`.
#[derive(Debug)]
pub struct Memstream {
    /// Where the program wants the buffer's address.
    bufp: *mut *mut c_char,
    /// Where the program wants the size.
    sizep: *mut usize,
    /// The buffer, from `malloc`, which the program frees.
    buf: *mut u8,
    /// The position.
    position: usize,
    /// The length of the contents.
    length: usize,
    /// The buffer's size.
    space: usize,
}

impl Memstream {
    /// Writes `len` bytes at the position, growing the buffer.
    ///
    /// # Safety
    ///
    /// `src` must be valid for reads of `len` bytes, outside the buffer.
    pub unsafe fn write(&mut self, src: *const u8, len: usize) -> Result<usize, c_int> {
        let Some(end) = self.position.checked_add(len) else {
            return Err(errno::ENOMEM);
        };
        if end >= self.space {
            let Some(wanted) = end.checked_add(1) else {
                return Err(errno::ENOMEM);
            };
            let space = self.space.saturating_mul(2).saturating_add(1).max(wanted);
            // SAFETY: the buffer came from `malloc` or `realloc`.
            let grown = unsafe { malloc::realloc(self.buf.cast(), space) }.cast::<u8>();
            if grown.is_null() {
                return Err(errno::ENOMEM);
            }
            // SAFETY: the new part of the buffer is `space - self.space` bytes.
            let _ = unsafe {
                string::memset(grown.wrapping_add(self.space).cast(), 0, space - self.space)
            };
            self.buf = grown;
            self.space = space;
            // SAFETY: `open_memstream` was given this pointer to store into.
            unsafe { self.bufp.write(grown.cast()) };
        }
        // SAFETY: the buffer has room up to `end`, and the caller vouches for
        // the source.
        let _ =
            unsafe { string::memcpy(self.buf.wrapping_add(self.position).cast(), src.cast(), len) };
        self.position = end;
        self.length = self.length.max(end);
        // SAFETY: `open_memstream` was given this pointer to store into.
        unsafe { self.sizep.write(end) };
        Ok(len)
    }

    /// Moves the position, which may pass the end.
    pub fn seek(&mut self, offset: i64, whence: c_int) -> Result<i64, c_int> {
        let base = base(whence, self.position, self.length)?;
        self.position = offset_within(base, offset, isize::MAX as usize)?;
        Ok(self.position as i64)
    }
}

/// Opens a stream that writes into a buffer it grows, storing the buffer's
/// address in `*bufp` and the size of what was written in `*sizep`.
///
/// # Safety
///
/// `bufp` and `sizep` must be valid for writes for as long as the stream is
/// open.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn open_memstream(bufp: *mut *mut c_char, sizep: *mut usize) -> *mut File {
    if bufp.is_null() || sizep.is_null() {
        errno::set(errno::EINVAL);
        return null_mut();
    }
    let buf = malloc::calloc(1, 1).cast::<u8>();
    if buf.is_null() {
        return null_mut();
    }
    let state = Memstream {
        bufp,
        sizep,
        buf,
        position: 0,
        length: 0,
        space: 1,
    };
    let file = file::allocate(Inner::new(
        Backend::Memstream(state),
        false,
        true,
        Mode::Unbuffered,
    ));
    if file.is_null() {
        // SAFETY: the buffer came from `calloc` and nothing else has it.
        unsafe { malloc::free(buf.cast()) };
        return null_mut();
    }
    // SAFETY: the caller passes pointers valid for writes.
    unsafe { bufp.write(buf.cast()) };
    // SAFETY: as above.
    unsafe { sizep.write(0) };
    file
}

/// The state of a stream from `fmemopen`.
#[derive(Debug)]
pub struct Fmem {
    /// The buffer.
    buf: *mut u8,
    /// The buffer's size.
    size: usize,
    /// The position.
    position: usize,
    /// The length of the contents.
    length: usize,
    /// Whether every write goes to the end, for mode `a`.
    append: bool,
    /// Whether the stream cannot be read.
    write_only: bool,
    /// Whether the buffer was allocated here.
    owned: bool,
}

impl Fmem {
    /// Reads up to `len` bytes from the position.
    ///
    /// # Safety
    ///
    /// `dst` must be valid for writes of `len` bytes, outside the buffer.
    pub unsafe fn read(&mut self, dst: *mut u8, len: usize) -> usize {
        let n = self.length.saturating_sub(self.position).min(len);
        // SAFETY: the `n` bytes after the position are inside the buffer, and
        // the caller vouches for the destination.
        let _ =
            unsafe { string::memcpy(dst.cast(), self.buf.wrapping_add(self.position).cast(), n) };
        self.position += n;
        n
    }

    /// Writes up to `len` bytes at the position, or at the end in append mode.
    ///
    /// # Safety
    ///
    /// `src` must be valid for reads of `len` bytes.
    pub unsafe fn write(&mut self, src: *const u8, len: usize) -> Result<usize, c_int> {
        if self.append {
            self.position = self.length;
        }
        let n = self.size.saturating_sub(self.position).min(len);
        if n == 0 {
            return Err(errno::ENOSPC);
        }
        // SAFETY: the `n` bytes after the position are inside the buffer, and
        // the caller vouches for the source.
        let _ =
            unsafe { string::memmove(self.buf.wrapping_add(self.position).cast(), src.cast(), n) };
        self.position += n;
        if self.position > self.length {
            self.length = self.position;
            if self.length < self.size {
                // SAFETY: `length` is inside the buffer.
                unsafe { self.buf.wrapping_add(self.length).write(0) };
            } else if self.write_only {
                // SAFETY: the buffer is not empty, since `n` was not zero.
                unsafe { self.buf.wrapping_add(self.size - 1).write(0) };
            }
        }
        Ok(n)
    }

    /// Moves the position, within the buffer.
    pub fn seek(&mut self, offset: i64, whence: c_int) -> Result<i64, c_int> {
        let base = base(whence, self.position, self.length)?;
        self.position = offset_within(base, offset, self.size)?;
        Ok(self.position as i64)
    }

    /// Frees a buffer allocated here.
    pub fn close(&mut self) {
        if self.owned {
            // SAFETY: the buffer came from `calloc` and only this stream has
            // it.
            unsafe { malloc::free(self.buf.cast()) };
            self.owned = false;
        }
    }
}

/// Opens a stream on the `size` bytes at `buf`, or on a buffer of its own if
/// `buf` is null.
///
/// # Safety
///
/// `mode` must be a NUL-terminated string, and `buf`, if not null, valid for
/// reads and writes of `size` bytes while the stream is open.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fmemopen(buf: *mut c_void, size: usize, mode: *const c_char) -> *mut File {
    // SAFETY: the caller passes a NUL-terminated mode.
    let Some(parsed) = (unsafe { parse_mode(mode) }) else {
        errno::set(errno::EINVAL);
        return null_mut();
    };
    let owned = buf.is_null();
    let buf = if owned {
        if size > isize::MAX as usize {
            errno::set(errno::ENOMEM);
            return null_mut();
        }
        let allocated = malloc::calloc(size.max(1), 1);
        if allocated.is_null() {
            return null_mut();
        }
        allocated.cast::<u8>()
    } else {
        buf.cast::<u8>()
    };
    let mut state = Fmem {
        buf,
        size,
        position: 0,
        length: 0,
        append: parsed.append,
        write_only: !parsed.readable,
        owned,
    };
    match parsed.first {
        b'r' => state.length = size,
        b'a' => {
            // SAFETY: the buffer holds `size` bytes.
            state.length = unsafe { string::strnlen(buf.cast(), size) };
            state.position = state.length;
        }
        _ => {
            if parsed.readable && size != 0 {
                // SAFETY: the buffer holds at least one byte.
                unsafe { buf.write(0) };
            }
        }
    }
    let mut inner = Inner::new(
        Backend::Fmem(state),
        parsed.readable,
        parsed.writable,
        Mode::Unbuffered,
    );
    inner.append = parsed.append;
    let file = file::allocate(inner);
    if file.is_null() && owned {
        // SAFETY: the buffer came from `calloc` and nothing else has it.
        unsafe { malloc::free(buf.cast()) };
    }
    file
}

/// A cookie stream's read function.
pub type CookieRead = unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> isize;
/// A cookie stream's write function.
pub type CookieWrite = unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> isize;
/// A cookie stream's seek function.
pub type CookieSeek = unsafe extern "C" fn(*mut c_void, *mut i64, c_int) -> c_int;
/// A cookie stream's close function.
pub type CookieClose = unsafe extern "C" fn(*mut c_void) -> c_int;

/// C's `cookie_io_functions_t`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CookieIoFunctions {
    /// Reads, or null.
    pub read: Option<CookieRead>,
    /// Writes, or null.
    pub write: Option<CookieWrite>,
    /// Seeks, or null.
    pub seek: Option<CookieSeek>,
    /// Closes, or null.
    pub close: Option<CookieClose>,
}

const _: () = assert!(size_of::<CookieIoFunctions>() == 32);

/// The state of a stream from `fopencookie`.
#[derive(Debug)]
pub struct Cookie {
    /// The program's pointer, passed to each function.
    cookie: *mut c_void,
    /// The program's functions.
    functions: CookieIoFunctions,
}

impl Cookie {
    /// Reads through the program's function. On failure `errno` is as the
    /// function left it.
    ///
    /// # Safety
    ///
    /// `dst` must be valid for writes of `len` bytes.
    pub(crate) unsafe fn read(&mut self, dst: *mut u8, len: usize) -> Result<usize, ()> {
        let Some(read) = self.functions.read else {
            errno::set(errno::EBADF);
            return Err(());
        };
        // SAFETY: the program supplied the function for this cookie, and the
        // caller vouches for the destination.
        let n = unsafe { read(self.cookie, dst.cast(), len) };
        usize::try_from(n).map_err(|_| ())
    }

    /// Writes through the program's function, or discards the output if it
    /// supplied none.
    ///
    /// # Safety
    ///
    /// `src` must be valid for reads of `len` bytes.
    pub(crate) unsafe fn write(&mut self, src: *const u8, len: usize) -> Result<usize, ()> {
        let Some(write) = self.functions.write else {
            return Ok(len);
        };
        // SAFETY: the program supplied the function for this cookie, and the
        // caller vouches for the source.
        let n = unsafe { write(self.cookie, src.cast(), len) };
        match usize::try_from(n) {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(()),
        }
    }

    /// Seeks through the program's function.
    pub(crate) fn seek(&mut self, offset: i64, whence: c_int) -> Result<i64, ()> {
        if !matches!(whence, SEEK_SET | SEEK_CUR | SEEK_END) {
            errno::set(errno::EINVAL);
            return Err(());
        }
        let Some(seek) = self.functions.seek else {
            errno::set(errno::ENOTSUP);
            return Err(());
        };
        let mut position = offset;
        // SAFETY: the program supplied the function for this cookie, and
        // `position` is a live local for it to update.
        if unsafe { seek(self.cookie, &raw mut position, whence) } < 0 {
            return Err(());
        }
        Ok(position)
    }

    /// Closes through the program's function.
    pub(crate) fn close(&mut self) -> Result<(), ()> {
        let Some(close) = self.functions.close else {
            return Ok(());
        };
        // SAFETY: the program supplied the function for this cookie.
        if unsafe { close(self.cookie) } < 0 {
            return Err(());
        }
        Ok(())
    }
}

/// Opens a stream whose reads, writes, seeks and close call `functions` with
/// `cookie`.
///
/// # Safety
///
/// `mode` must be a NUL-terminated string, and the functions must be sound to
/// call with `cookie` while the stream is open.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fopencookie(
    cookie: *mut c_void,
    mode: *const c_char,
    functions: CookieIoFunctions,
) -> *mut File {
    // SAFETY: the caller passes a NUL-terminated mode.
    let Some(parsed) = (unsafe { parse_mode(mode) }) else {
        errno::set(errno::EINVAL);
        return null_mut();
    };
    let mut inner = Inner::new(
        Backend::Cookie(Cookie { cookie, functions }),
        parsed.readable,
        parsed.writable,
        Mode::Full,
    );
    inner.append = parsed.append;
    file::allocate(inner)
}
