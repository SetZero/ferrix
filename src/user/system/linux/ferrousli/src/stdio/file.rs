//! The `FILE` structure: a backend, a buffer, and the state of reading and
//! writing through it.
//!
//! # Backends
//!
//! A stream reads, writes, seeks and closes through a [`Backend`]: a
//! descriptor, a memory stream, a fixed memory buffer, or a program's own
//! functions. Everything above that, the buffering, `ungetc` and the end-of-file
//! and error indicators, is the same for all of them.
//!
//! # Buffering
//!
//! A stream is idle, reading, or writing.
//!
//! * Writing, bytes collect in the buffer until it is full, until a newline if
//!   the stream is line-buffered, or until something flushes it. A write too
//!   large for the buffer goes straight to the backend once the buffer is
//!   flushed. An unbuffered stream writes every call through.
//! * Reading, the buffer is refilled with one backend read when it is empty. A
//!   read larger than the buffer goes straight into the caller's memory. An
//!   unbuffered stream reads a byte at a time for the character functions.
//!
//! The buffer is allocated at the first read or write, so `setvbuf` before any
//! I/O never allocates. That is also when a stream whose mode was not chosen
//! is resolved: line-buffered if its descriptor is a terminal, which `TCGETS`
//! answers, and fully buffered otherwise. Standard error is unbuffered.
//!
//! Before a read on a line-buffered or unbuffered descriptor asks the kernel
//! for more, pending output on a line-buffered standard output is flushed, so
//! that a prompt appears before the program waits for its answer.
//!
//! # Pushback
//!
//! `ungetc` pushes bytes onto an array of [`UNGET`] bytes of the stream's own,
//! which reads take from before the buffer. It never writes into the buffer, so
//! it works over a buffered read and over a buffer `setvbuf` supplied, and it
//! cannot reach memory outside either. A seek discards it.
//!
//! # Locking
//!
//! Each stream has a [`RecursiveLock`]. The functions take it, run on the state
//! with it held, and release it; the `_unlocked` functions skip it. The state
//! is an `UnsafeCell` that is only reached through [`locked`] and
//! [`unlocked`]. A backend function a program supplied to `fopencookie` that
//! uses its own stream again, while that stream is in a call, is undefined, as
//! it is in C.
//!
//! # The open stream list
//!
//! Streams from `fopen` and its relatives are linked into a list, under a lock
//! of its own, for `fflush(NULL)` and `exit` to walk. The three standard
//! streams are statics, outside the list, and are never freed: `fclose` on one
//! closes its descriptor and leaves the structure closed.

use core::cell::UnsafeCell;
use core::ffi::{c_int, c_void};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

use super::lock::RecursiveLock;
use super::memory::{Cookie, Fmem, Memstream, WMemstream};
use super::sys;
use crate::{errno, malloc, string};

/// The size of a buffer allocated for a stream.
pub const BUFFER_SIZE: usize = 4096;
/// How many bytes `ungetc` can push back.
pub const UNGET: usize = 8;

/// `SEEK_SET`.
pub const SEEK_SET: c_int = 0;
/// `SEEK_CUR`.
pub const SEEK_CUR: c_int = 1;
/// `SEEK_END`.
pub const SEEK_END: c_int = 2;

/// How a stream buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Not chosen yet: resolved at the first I/O.
    Auto,
    /// Every write goes through.
    Unbuffered,
    /// Output is flushed after each newline.
    Line,
    /// Output is flushed when the buffer is full.
    Full,
}

/// What a stream is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io {
    /// Neither: the buffer holds nothing.
    Idle,
    /// The buffer and the pushback hold input not yet consumed.
    Reading,
    /// The buffer holds output not yet written.
    Writing,
}

/// Where a stream's bytes go and come from.
#[derive(Debug)]
pub enum Backend {
    /// Closed: every operation fails.
    Closed,
    /// A file descriptor.
    Fd(c_int),
    /// A growing buffer from `open_memstream`.
    Memstream(Memstream),
    /// A growing wide buffer from `open_wmemstream`.
    WMemstream(WMemstream),
    /// A fixed buffer from `fmemopen`.
    Fmem(Fmem),
    /// Functions from `fopencookie`.
    Cookie(Cookie),
}

impl Backend {
    /// Reads up to `len` bytes into `dst`, returning how many, zero at the
    /// end. On failure `errno` is set.
    ///
    /// # Safety
    ///
    /// `dst` must be valid for writes of `len` bytes.
    unsafe fn read(&mut self, dst: *mut u8, len: usize) -> Result<usize, ()> {
        let result = match self {
            Self::Closed => Err(errno::EBADF),
            // SAFETY: the caller vouches for the destination.
            Self::Fd(fd) => unsafe { sys::read(*fd, dst, len) },
            Self::Memstream(_) | Self::WMemstream(_) => Err(errno::EBADF),
            // SAFETY: as above.
            Self::Fmem(fmem) => Ok(unsafe { fmem.read(dst, len) }),
            // SAFETY: as above.
            Self::Cookie(cookie) => return unsafe { cookie.read(dst, len) },
        };
        result.map_err(errno::set)
    }

    /// Writes some of the `len` bytes at `src`, returning how many, at least
    /// one. On failure `errno` is set.
    ///
    /// # Safety
    ///
    /// `src` must be valid for reads of `len` bytes, and `len` not zero.
    unsafe fn write(&mut self, src: *const u8, len: usize) -> Result<usize, ()> {
        let result = match self {
            Self::Closed => Err(errno::EBADF),
            // SAFETY: the caller vouches for the source.
            Self::Fd(fd) => match unsafe { sys::write(*fd, src, len) } {
                // The kernel accepted nothing of a non-empty write.
                Ok(0) => Err(errno::EIO),
                other => other,
            },
            // SAFETY: as above.
            Self::Memstream(memstream) => unsafe { memstream.write(src, len) },
            // SAFETY: as above.
            Self::WMemstream(memstream) => unsafe { memstream.write(src, len) },
            // SAFETY: as above.
            Self::Fmem(fmem) => unsafe { fmem.write(src, len) },
            // SAFETY: as above.
            Self::Cookie(cookie) => return unsafe { cookie.write(src, len) },
        };
        result.map_err(errno::set)
    }

    /// Moves the position, returning the new one. On failure `errno` is set.
    fn seek(&mut self, offset: i64, whence: c_int) -> Result<i64, ()> {
        let result = match self {
            Self::Closed => Err(errno::EBADF),
            Self::Fd(fd) => sys::lseek(*fd, offset, whence),
            Self::Memstream(memstream) => memstream.seek(offset, whence),
            Self::WMemstream(memstream) => memstream.seek(offset, whence),
            Self::Fmem(fmem) => fmem.seek(offset, whence),
            Self::Cookie(cookie) => return cookie.seek(offset, whence),
        };
        result.map_err(errno::set)
    }

    /// Releases what the backend holds. On failure `errno` is set.
    fn close(&mut self) -> Result<(), ()> {
        let result = match self {
            Self::Closed | Self::Memstream(_) | Self::WMemstream(_) => Ok(()),
            Self::Fd(fd) => sys::close(*fd),
            Self::Fmem(fmem) => {
                fmem.close();
                Ok(())
            }
            Self::Cookie(cookie) => return cookie.close(),
        };
        result.map_err(errno::set)
    }
}

/// A stream's state, reached with its lock held.
#[derive(Debug)]
pub struct Inner {
    /// Where the bytes go and come from.
    pub backend: Backend,
    /// Whether the stream may be read.
    pub readable: bool,
    /// Whether the stream may be written.
    pub writable: bool,
    /// Whether every write goes to the end of the file.
    pub append: bool,
    /// The end-of-file indicator.
    pub eof: bool,
    /// The error indicator.
    pub error: bool,
    /// The shell `popen` started on this stream, whose exit `pclose` waits
    /// for, or 0.
    pub pipe_pid: c_int,
    /// How the stream buffers.
    mode: Mode,
    /// Whether the mode was chosen, by `setvbuf` or when the stream was made,
    /// rather than left to be resolved at the first I/O.
    chosen: bool,
    /// The buffer, or null until it is needed.
    buf: *mut u8,
    /// The buffer's size, or the size to allocate while it is null. Zero while
    /// null means [`BUFFER_SIZE`].
    cap: usize,
    /// Whether the buffer came from `malloc` here, to be freed.
    owned: bool,
    /// What the stream is doing.
    io: Io,
    /// While reading, the next unread byte of the buffer.
    rpos: usize,
    /// While reading, the end of the valid bytes in the buffer.
    rend: usize,
    /// While writing, the bytes of output in the buffer.
    wlen: usize,
    /// Pushed-back bytes, in the last `pushed` places, in reading order.
    pushback: [u8; UNGET],
    /// How many bytes are pushed back.
    pushed: usize,
    /// The orientation `fwide` reports: 0 until the stream is used for wide
    /// or byte I/O, positive once wide, negative once byte-oriented.
    pub orientation: i8,
}

impl Inner {
    /// A stream on `backend`, with nothing buffered.
    pub const fn new(backend: Backend, readable: bool, writable: bool, mode: Mode) -> Self {
        Self {
            backend,
            readable,
            writable,
            append: false,
            eof: false,
            error: false,
            pipe_pid: 0,
            mode,
            chosen: !matches!(mode, Mode::Auto),
            buf: null_mut(),
            cap: 0,
            owned: false,
            io: Io::Idle,
            rpos: 0,
            rend: 0,
            wlen: 0,
            pushback: [0; UNGET],
            pushed: 0,
            orientation: 0,
        }
    }

    /// Sets the error indicator and `errno`.
    pub fn fail(&mut self, error: c_int) {
        self.error = true;
        errno::set(error);
    }

    /// The buffering mode, resolving [`Mode::Auto`].
    fn resolve_mode(&mut self) -> Mode {
        if self.mode == Mode::Auto {
            self.mode = match self.backend {
                Backend::Fd(fd) if sys::is_terminal(fd) => Mode::Line,
                _ => Mode::Full,
            };
        }
        self.mode
    }

    /// Makes sure a buffered stream has its buffer. False if the stream is
    /// unbuffered, or has become so because no buffer could be allocated.
    fn has_buffer(&mut self) -> bool {
        if self.resolve_mode() == Mode::Unbuffered {
            return false;
        }
        if self.buf.is_null() {
            let size = if self.cap == 0 { BUFFER_SIZE } else { self.cap };
            let buf = malloc::malloc(size).cast::<u8>();
            if buf.is_null() {
                self.mode = Mode::Unbuffered;
                self.cap = 0;
                return false;
            }
            self.buf = buf;
            self.cap = size;
            self.owned = true;
        }
        true
    }

    /// Frees a buffer allocated here, and forgets any buffer.
    fn release_buffer(&mut self) {
        if self.owned {
            // SAFETY: the buffer came from `malloc` and nothing else holds it.
            unsafe { malloc::free(self.buf.cast()) };
        }
        self.buf = null_mut();
        self.cap = 0;
        self.owned = false;
    }

    /// Hands unread input back to the backend, so that its position is where
    /// the program has read to, and empties the read state. If the backend
    /// cannot seek, the input is kept, and false returned.
    fn sync_read(&mut self) -> bool {
        let unread = self.rend.saturating_sub(self.rpos) + self.pushed;
        if unread != 0 {
            // SAFETY: the pointer is this thread's `errno`.
            let saved = unsafe { errno::__errno_location().read() };
            let seek = self.backend.seek(-(unread as i64), SEEK_CUR);
            errno::set(saved);
            if seek.is_err() {
                return false;
            }
        }
        self.rpos = 0;
        self.rend = 0;
        self.pushed = 0;
        self.io = Io::Idle;
        true
    }

    /// Writes all `len` bytes at `src` to the backend, returning how many
    /// were written. A shortfall sets the error indicator.
    ///
    /// # Safety
    ///
    /// `src` must be valid for reads of `len` bytes.
    unsafe fn write_through(&mut self, src: *const u8, len: usize) -> usize {
        let mut done = 0;
        while done < len {
            // SAFETY: the caller vouches for `len` bytes at `src`.
            match unsafe { self.backend.write(src.wrapping_add(done), len - done) } {
                Ok(n) => done += n.min(len - done),
                Err(()) => {
                    self.error = true;
                    break;
                }
            }
        }
        done
    }

    /// Writes the buffered output. On failure the output is dropped, and the
    /// error indicator set.
    pub fn flush_output(&mut self) -> bool {
        let len = self.wlen;
        self.wlen = 0;
        if len == 0 {
            return true;
        }
        // SAFETY: the first `len` bytes of the buffer are the output.
        unsafe { self.write_through(self.buf, len) == len }
    }

    /// Prepares to write. False, with the error indicator set, if the stream
    /// cannot be written.
    fn start_writing(&mut self) -> bool {
        if self.io == Io::Writing {
            return true;
        }
        if !self.writable {
            self.fail(errno::EBADF);
            return false;
        }
        if self.io == Io::Reading {
            let _ = self.sync_read();
            self.rpos = 0;
            self.rend = 0;
            self.pushed = 0;
        }
        self.io = Io::Writing;
        self.wlen = 0;
        true
    }

    /// Writes `len` bytes from `src`, returning how many the stream took.
    ///
    /// # Safety
    ///
    /// `src` must be valid for reads of `len` bytes.
    pub unsafe fn write(&mut self, src: *const u8, len: usize) -> usize {
        if len == 0 || !self.start_writing() {
            return 0;
        }
        if self.resolve_mode() != Mode::Line {
            // SAFETY: the caller vouches for the source.
            return unsafe { self.write_buffered(src, len) };
        }
        let mut last_newline = None;
        let mut i = len;
        while i > 0 {
            i -= 1;
            // SAFETY: `i` is below `len`.
            if unsafe { src.wrapping_add(i).read() } == b'\n' {
                last_newline = Some(i);
                break;
            }
        }
        let Some(newline) = last_newline else {
            // SAFETY: the caller vouches for the source.
            return unsafe { self.write_buffered(src, len) };
        };
        let head = newline + 1;
        // SAFETY: `head` is at most `len`.
        let written = unsafe { self.write_buffered(src, head) };
        if written < head {
            return written;
        }
        if !self.flush_output() {
            return 0;
        }
        // SAFETY: the rest of the caller's bytes.
        head + unsafe { self.write_buffered(src.wrapping_add(head), len - head) }
    }

    /// Writes through the buffer, ignoring line buffering.
    ///
    /// # Safety
    ///
    /// `src` must be valid for reads of `len` bytes.
    unsafe fn write_buffered(&mut self, src: *const u8, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        if !self.has_buffer() {
            if !self.flush_output() {
                return 0;
            }
            // SAFETY: the caller vouches for the source.
            return unsafe { self.write_through(src, len) };
        }
        if len > self.cap - self.wlen {
            if !self.flush_output() {
                return 0;
            }
            if len >= self.cap {
                // SAFETY: the caller vouches for the source.
                return unsafe { self.write_through(src, len) };
            }
        }
        // SAFETY: the buffer has room for `len` more bytes, and the caller's
        // bytes are not inside it.
        let _ = unsafe { string::memcpy(self.buf.wrapping_add(self.wlen).cast(), src.cast(), len) };
        self.wlen += len;
        len
    }

    /// Writes one byte, taking a short path when it only lands in the buffer.
    pub fn put_byte(&mut self, byte: u8) -> bool {
        let fits = self.io == Io::Writing && self.wlen < self.cap && !self.buf.is_null();
        let no_flush = match self.mode {
            Mode::Full => true,
            Mode::Line => byte != b'\n',
            Mode::Auto | Mode::Unbuffered => false,
        };
        if fits && no_flush {
            // SAFETY: `wlen` is inside the buffer.
            unsafe { self.buf.wrapping_add(self.wlen).write(byte) };
            self.wlen += 1;
            return true;
        }
        // SAFETY: `byte` is a live local.
        unsafe { self.write(&raw const byte, 1) == 1 }
    }

    /// Prepares to read. False, with the error indicator set, if the stream
    /// cannot be read or its pending output cannot be written.
    fn start_reading(&mut self) -> bool {
        if self.io == Io::Reading {
            return true;
        }
        if !self.readable {
            self.fail(errno::EBADF);
            return false;
        }
        if self.io == Io::Writing {
            self.io = Io::Idle;
            if !self.flush_output() {
                return false;
            }
        }
        self.io = Io::Reading;
        self.rpos = 0;
        self.rend = 0;
        true
    }

    /// Flushes standard output first, if this stream is about to wait on a
    /// line-buffered or unbuffered descriptor and standard output is
    /// line-buffered.
    fn flush_before_waiting(&mut self) {
        if !matches!(self.backend, Backend::Fd(_))
            || !matches!(self.mode, Mode::Line | Mode::Unbuffered)
        {
            return;
        }
        let out = &STDOUT_FILE;
        if core::ptr::eq(self, out.inner.get()) || !out.lock.try_lock_unowned() {
            return;
        }
        // SAFETY: standard output's lock is held, and no call on it was in
        // progress on this thread, since the lock was unowned.
        let inner = unsafe { &mut *out.inner.get() };
        if inner.mode == Mode::Line && inner.io == Io::Writing {
            let _ = inner.flush_output();
        }
        out.lock.unlock();
    }

    /// The input available without asking the backend: the pushback, else the
    /// buffer. Refills from the backend when both are empty. `None` at the end
    /// of the file or on an error, which set their indicators.
    pub fn peek(&mut self) -> Option<(*const u8, usize)> {
        if self.io == Io::Reading {
            if self.pushed != 0 {
                let at = self.pushback.as_ptr().wrapping_add(UNGET - self.pushed);
                return Some((at, self.pushed));
            }
            if self.rpos < self.rend {
                return Some((self.buf.wrapping_add(self.rpos), self.rend - self.rpos));
            }
        }
        if self.eof || !self.start_reading() {
            return None;
        }
        let buffered = self.has_buffer();
        self.flush_before_waiting();
        if buffered {
            // SAFETY: the buffer holds `cap` bytes.
            match unsafe { self.backend.read(self.buf, self.cap) } {
                Ok(0) => self.eof = true,
                Ok(n) => {
                    self.rpos = 0;
                    self.rend = n.min(self.cap);
                    return Some((self.buf, self.rend));
                }
                Err(()) => self.error = true,
            }
            return None;
        }
        let mut byte = 0_u8;
        // SAFETY: `byte` is a live local.
        match unsafe { self.backend.read(&raw mut byte, 1) } {
            Ok(0) => self.eof = true,
            Ok(_) => {
                if let Some(slot) = self.pushback.last_mut() {
                    *slot = byte;
                    self.pushed = 1;
                    return Some((self.pushback.as_ptr().wrapping_add(UNGET - 1), 1));
                }
            }
            Err(()) => self.error = true,
        }
        None
    }

    /// Consumes `n` bytes of what [`Self::peek`] last returned.
    pub fn consume(&mut self, n: usize) {
        if self.pushed != 0 {
            self.pushed -= n.min(self.pushed);
        } else {
            self.rpos = (self.rpos + n).min(self.rend);
        }
    }

    /// Reads one byte.
    pub fn get_byte(&mut self) -> Option<u8> {
        let (at, _) = self.peek()?;
        // SAFETY: `peek` returned at least one readable byte at `at`.
        let byte = unsafe { at.read() };
        self.consume(1);
        Some(byte)
    }

    /// Reads up to `len` bytes into `dst`, returning how many.
    ///
    /// # Safety
    ///
    /// `dst` must be valid for writes of `len` bytes.
    pub unsafe fn read(&mut self, dst: *mut u8, len: usize) -> usize {
        let mut done = 0;
        while done < len {
            let available = self.io == Io::Reading && (self.pushed != 0 || self.rpos < self.rend);
            if available && let Some((at, n)) = self.peek() {
                let take = n.min(len - done);
                // SAFETY: `peek` returned `n` readable bytes, and the caller
                // vouches for `len` writable ones.
                let _ = unsafe { string::memcpy(dst.wrapping_add(done).cast(), at.cast(), take) };
                self.consume(take);
                done += take;
                continue;
            }
            if self.eof {
                break;
            }
            let want = len - done;
            if !self.has_buffer() || want >= self.cap {
                if !self.start_reading() {
                    break;
                }
                self.flush_before_waiting();
                // SAFETY: the caller vouches for the rest of the destination.
                match unsafe { self.backend.read(dst.wrapping_add(done), want) } {
                    Ok(0) => {
                        self.eof = true;
                        break;
                    }
                    Ok(n) => done += n.min(want),
                    Err(()) => {
                        self.error = true;
                        break;
                    }
                }
                continue;
            }
            if self.peek().is_none() {
                break;
            }
        }
        done
    }

    /// Pushes `bytes` back so that they are read next, in order, or nothing if
    /// they do not all fit or the stream cannot be read. `ungetwc` pushes a
    /// whole character's bytes this way.
    pub fn unget_bytes(&mut self, bytes: &[u8]) -> bool {
        if !self.start_reading() || UNGET - self.pushed < bytes.len() {
            return false;
        }
        for &byte in bytes.iter().rev() {
            let _ = self.unget(byte);
        }
        true
    }

    /// Pushes `byte` back. False if the pushback is full or the stream cannot
    /// be read.
    pub fn unget(&mut self, byte: u8) -> bool {
        if !self.start_reading() || self.pushed == UNGET {
            return false;
        }
        self.pushed += 1;
        if let Some(slot) = self.pushback.get_mut(UNGET - self.pushed) {
            *slot = byte;
        }
        self.eof = false;
        true
    }

    /// The bytes of output waiting in the buffer, for `__fpending`.
    pub fn pending_output(&self) -> usize {
        if self.io == Io::Writing { self.wlen } else { 0 }
    }

    /// The bytes of input already read from the backend and not yet
    /// consumed, pushback included, for `__freadahead`.
    pub fn read_ahead(&self) -> usize {
        if self.io == Io::Reading {
            self.pushed + self.rend.saturating_sub(self.rpos)
        } else {
            0
        }
    }

    /// Whether the last operation was a read, for `__freading`.
    pub fn is_reading(&self) -> bool {
        self.io == Io::Reading
    }

    /// Whether the last operation was a write, for `__fwriting`.
    pub fn is_writing(&self) -> bool {
        self.io == Io::Writing
    }

    /// Whether output is flushed at each newline, for `__flbf`.
    pub fn is_line_buffered(&mut self) -> bool {
        self.resolve_mode() == Mode::Line
    }

    /// The buffer's size, or 0 before one is allocated, for `__fbufsize`.
    pub fn buffer_size(&self) -> usize {
        if self.buf.is_null() { 0 } else { self.cap }
    }

    /// Discards buffered input and output without writing or giving back
    /// either, as `__fpurge` does.
    pub fn purge(&mut self) {
        self.io = Io::Idle;
        self.wlen = 0;
        self.rpos = 0;
        self.rend = 0;
        self.pushed = 0;
    }

    /// Writes pending output, or gives unread input back to a seekable
    /// backend. False if output could not be written.
    pub fn flush(&mut self) -> bool {
        match self.io {
            Io::Writing => {
                self.io = Io::Idle;
                self.flush_output()
            }
            Io::Reading => {
                let _ = self.sync_read();
                true
            }
            Io::Idle => true,
        }
    }

    /// Moves the position, discarding buffered input and pushback and clearing
    /// the end-of-file indicator. False, with `errno` set, on failure.
    pub fn seek(&mut self, offset: i64, whence: c_int) -> bool {
        if !matches!(whence, SEEK_SET | SEEK_CUR | SEEK_END) {
            errno::set(errno::EINVAL);
            return false;
        }
        let mut offset = offset;
        match self.io {
            Io::Writing => {
                self.io = Io::Idle;
                if !self.flush_output() {
                    return false;
                }
            }
            Io::Reading if whence == SEEK_CUR => {
                let unread = (self.rend.saturating_sub(self.rpos) + self.pushed) as i64;
                let Some(adjusted) = offset.checked_sub(unread) else {
                    errno::set(errno::EOVERFLOW);
                    return false;
                };
                offset = adjusted;
            }
            Io::Reading | Io::Idle => {}
        }
        if self.backend.seek(offset, whence).is_err() {
            return false;
        }
        self.io = Io::Idle;
        self.rpos = 0;
        self.rend = 0;
        self.pushed = 0;
        self.wlen = 0;
        self.eof = false;
        true
    }

    /// The position as the program sees it, counting buffered data, or -1
    /// with `errno` set.
    pub fn tell(&mut self) -> i64 {
        let whence = if self.append && self.io == Io::Writing && self.wlen != 0 {
            SEEK_END
        } else {
            SEEK_CUR
        };
        let Ok(position) = self.backend.seek(0, whence) else {
            return -1;
        };
        let position = match self.io {
            Io::Reading => position - (self.rend.saturating_sub(self.rpos) + self.pushed) as i64,
            Io::Writing => position + self.wlen as i64,
            Io::Idle => position,
        };
        if position < 0 {
            errno::set(errno::EINVAL);
            return -1;
        }
        position
    }

    /// Chooses the buffering: `mode`, with `size` bytes at `buf` if `buf` is
    /// not null, or `size` bytes allocated at the first I/O.
    pub fn set_buffer(&mut self, buf: *mut u8, mode: Mode, size: usize) {
        let _ = self.flush();
        self.io = Io::Idle;
        self.rpos = 0;
        self.rend = 0;
        self.wlen = 0;
        self.release_buffer();
        self.mode = mode;
        self.chosen = true;
        if mode != Mode::Unbuffered {
            self.cap = size;
            if !buf.is_null() && size != 0 {
                self.buf = buf;
            }
        }
    }

    /// Flushes and closes the stream, and frees its buffer. False if either
    /// the flush or the close failed.
    pub fn close(&mut self) -> bool {
        let flushed = self.flush();
        let closed = self.backend.close().is_ok();
        self.backend = Backend::Closed;
        self.release_buffer();
        self.io = Io::Idle;
        self.rpos = 0;
        self.rend = 0;
        self.wlen = 0;
        self.pushed = 0;
        flushed && closed
    }

    /// Reopens the stream on `backend`, as `freopen` does: the indicators and
    /// pushback are cleared, and a mode `setvbuf` did not choose is resolved
    /// again.
    pub fn reset(&mut self, readable: bool, writable: bool, append: bool) {
        self.readable = readable;
        self.writable = writable;
        self.append = append;
        self.eof = false;
        self.error = false;
        self.io = Io::Idle;
        self.rpos = 0;
        self.rend = 0;
        self.wlen = 0;
        self.pushed = 0;
        self.orientation = 0;
        if !self.chosen {
            self.mode = Mode::Auto;
        }
    }
}

/// The start of glibc's `struct _IO_FILE`, which glibc's headers read inline.
///
/// A program built against glibc with optimisation expands `getc_unlocked`,
/// `putc_unlocked`, `feof_unlocked` and `ferror_unlocked` into reads of the
/// stream's own fields: the next byte is `*_IO_read_ptr++` unless the read
/// pointer has reached `_IO_read_end`, when it calls `__uflow`, and writing
/// is the same with `_IO_write_ptr`, `_IO_write_end` and `__overflow`. GLib
/// is built that way. So a `FILE` begins as glibc's does. Every pointer here
/// stays null, so each comparison finds the buffer used up and calls the
/// function, which takes the byte from this library's own buffer; and
/// `_flags` mirrors the end-of-file and error indicators, which the other
/// two read, after every call on the stream.
#[repr(C)]
#[derive(Debug)]
struct GlibcHead {
    /// `_flags`: [`GLIBC_EOF_SEEN`] and [`GLIBC_ERR_SEEN`].
    flags: AtomicI32,
    /// `_IO_read_ptr` to `_IO_buf_end`, all null.
    pointers: [usize; 8],
}

/// glibc's `_IO_EOF_SEEN`.
const GLIBC_EOF_SEEN: i32 = 0x10;
/// glibc's `_IO_ERR_SEEN`.
const GLIBC_ERR_SEEN: i32 = 0x20;

// `_flags` is an `int` at the start, and `_IO_read_ptr` the word after it,
// as glibc's headers compile the offsets in.
const _: () = assert!(core::mem::offset_of!(GlibcHead, pointers) == size_of::<usize>());

/// C's `FILE`.
#[repr(C)]
#[derive(Debug)]
pub struct File {
    /// What a program built against glibc reads without a call. First.
    glibc: GlibcHead,
    /// The lock `flockfile` takes.
    lock: RecursiveLock,
    /// The next stream in the open list, under [`LIST_LOCK`].
    next: AtomicPtr<File>,
    /// The previous stream in the open list, under [`LIST_LOCK`].
    prev: AtomicPtr<File>,
    /// The state, reached with the lock held.
    inner: UnsafeCell<Inner>,
}

// SAFETY: the links are atomics under the list's lock, and the state in the
// cell is only reached through `locked` and `unlocked`, whose callers hold or
// have taken responsibility for the stream's lock.
unsafe impl Sync for File {}

impl File {
    /// A stream with the given state.
    pub const fn new(inner: Inner) -> Self {
        Self {
            glibc: GlibcHead {
                flags: AtomicI32::new(0),
                pointers: [0; 8],
            },
            lock: RecursiveLock::new(),
            next: AtomicPtr::new(null_mut()),
            prev: AtomicPtr::new(null_mut()),
            inner: UnsafeCell::new(inner),
        }
    }

    /// The stream's lock.
    pub fn lock(&self) -> &RecursiveLock {
        &self.lock
    }

    /// Copies `inner`'s indicators into glibc's `_flags`.
    fn mirror(&self, inner: &Inner) {
        let mut flags = 0;
        if inner.eof {
            flags |= GLIBC_EOF_SEEN;
        }
        if inner.error {
            flags |= GLIBC_ERR_SEEN;
        }
        self.glibc.flags.store(flags, Ordering::Relaxed);
    }
}

/// Standard input's stream.
pub static STDIN_FILE: File = File::new(Inner::new(Backend::Fd(0), true, false, Mode::Auto));
/// Standard output's stream.
pub static STDOUT_FILE: File = File::new(Inner::new(Backend::Fd(1), false, true, Mode::Auto));
/// Standard error's stream, unbuffered.
pub static STDERR_FILE: File = File::new(Inner::new(Backend::Fd(2), false, true, Mode::Unbuffered));

/// `stdin`. C declares it `FILE *const`; glibc programs may assign to it.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static stdin: AtomicPtr<File> = AtomicPtr::new((&raw const STDIN_FILE).cast_mut());
/// `stdout`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static stdout: AtomicPtr<File> = AtomicPtr::new((&raw const STDOUT_FILE).cast_mut());
/// `stderr`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static stderr: AtomicPtr<File> = AtomicPtr::new((&raw const STDERR_FILE).cast_mut());

/// Guards the open stream list.
static LIST_LOCK: RecursiveLock = RecursiveLock::new();
/// The newest stream in the open list.
static OPEN: AtomicPtr<File> = AtomicPtr::new(null_mut());

/// Whether `file` is one of the three standard streams.
pub fn is_standard(file: *const File) -> bool {
    [&STDIN_FILE, &STDOUT_FILE, &STDERR_FILE]
        .iter()
        .any(|standard| core::ptr::eq(*standard, file))
}

/// Runs `op` on the stream's state with its lock held.
///
/// # Safety
///
/// `file` must be a live stream.
pub unsafe fn locked<R>(file: *mut File, op: impl FnOnce(&mut Inner) -> R) -> R {
    // SAFETY: the caller passes a live stream.
    let file = unsafe { &*file };
    file.lock.lock();
    // SAFETY: the lock is held, so no other thread is using the state.
    let inner = unsafe { &mut *file.inner.get() };
    let result = op(&mut *inner);
    file.mirror(inner);
    file.lock.unlock();
    result
}

/// Runs `op` on the stream's state without taking its lock.
///
/// # Safety
///
/// `file` must be a live stream, and the caller must hold its lock or know
/// that no other thread uses it.
pub unsafe fn unlocked<R>(file: *mut File, op: impl FnOnce(&mut Inner) -> R) -> R {
    // SAFETY: the caller passes a live stream.
    let file = unsafe { &*file };
    // SAFETY: the caller vouches that nothing else uses its state.
    let inner = unsafe { &mut *file.inner.get() };
    let result = op(&mut *inner);
    file.mirror(inner);
    result
}

/// Allocates a stream with the state `inner` and links it into the open list.
/// Null, with `errno` set, if there is no memory; the backend is then the
/// caller's to release.
pub fn allocate(inner: Inner) -> *mut File {
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "malloc aligns to 16, more than a File needs"
    )]
    let file = malloc::malloc(size_of::<File>()).cast::<File>();
    if file.is_null() {
        return file;
    }
    // SAFETY: the memory is fresh, aligned and large enough for a `File`.
    unsafe { file.write(File::new(inner)) };
    LIST_LOCK.lock();
    let head = OPEN.load(Ordering::Relaxed);
    // SAFETY: `file` was just initialised.
    let this = unsafe { &*file };
    this.next.store(head, Ordering::Relaxed);
    if !head.is_null() {
        // SAFETY: streams in the list are live.
        unsafe { &*head }.prev.store(file, Ordering::Relaxed);
    }
    OPEN.store(file, Ordering::Relaxed);
    LIST_LOCK.unlock();
    file
}

/// Unlinks a stream from the open list and frees it. A standard stream is
/// left alone.
///
/// # Safety
///
/// `file` must be a closed stream that nothing will use again.
pub unsafe fn free(file: *mut File) {
    if is_standard(file) {
        return;
    }
    LIST_LOCK.lock();
    // SAFETY: the caller passes a live stream.
    let this = unsafe { &*file };
    let next = this.next.load(Ordering::Relaxed);
    let prev = this.prev.load(Ordering::Relaxed);
    if !next.is_null() {
        // SAFETY: streams in the list are live.
        unsafe { &*next }.prev.store(prev, Ordering::Relaxed);
    }
    if prev.is_null() {
        OPEN.store(next, Ordering::Relaxed);
    } else {
        // SAFETY: as above.
        unsafe { &*prev }.next.store(next, Ordering::Relaxed);
    }
    LIST_LOCK.unlock();
    // SAFETY: the stream came from `malloc` in `allocate`, and is unlinked.
    unsafe { malloc::free(file.cast::<c_void>()) };
}

/// Runs `op` on every open stream's state, each with its lock held: the
/// standard streams, then the list, newest first.
fn for_each(mut op: impl FnMut(&mut Inner)) {
    for standard in [&STDIN_FILE, &STDOUT_FILE, &STDERR_FILE] {
        let file = core::ptr::from_ref(standard).cast_mut();
        // SAFETY: the standard streams are statics.
        unsafe { locked(file, &mut op) };
    }
    LIST_LOCK.lock();
    let mut at = OPEN.load(Ordering::Relaxed);
    while !at.is_null() {
        // SAFETY: streams in the list are live while the list lock is held.
        unsafe { locked(at, &mut op) };
        // SAFETY: as above.
        at = unsafe { &*at }.next.load(Ordering::Relaxed);
    }
    LIST_LOCK.unlock();
}

/// Takes the open list's lock, for `popen`, which forks while holding it so
/// that the child's copy of the list is whole.
pub fn lock_list() {
    LIST_LOCK.lock();
}

/// Releases what [`lock_list`] took.
pub fn unlock_list() {
    LIST_LOCK.unlock();
}

/// Closes the descriptor of every stream an earlier `popen` opened, in the
/// child of a `popen`.
///
/// # Safety
///
/// Only the child of a `fork` made while [`lock_list`] was held may call this,
/// before it runs anything else. It reads the streams without their locks,
/// which threads the child does not have may have held at the fork.
pub unsafe fn close_popen_descriptors() {
    let mut at = OPEN.load(Ordering::Relaxed);
    while !at.is_null() {
        // SAFETY: the list was whole at the fork, and its streams live.
        let this = unsafe { &*at };
        // SAFETY: the child has one thread, and nothing else reads this copy.
        let inner = unsafe { &*this.inner.get() };
        if inner.pipe_pid != 0
            && let Backend::Fd(fd) = inner.backend
        {
            let _ = crate::unistd::close(fd);
        }
        at = this.next.load(Ordering::Relaxed);
    }
}

/// Writes every stream's pending output, for `fflush(NULL)`. False if any
/// failed.
pub fn flush_all() -> bool {
    let mut ok = true;
    for_each(|inner| {
        if inner.io == Io::Writing {
            ok &= inner.flush();
        }
    });
    ok
}

/// Flushes every stream as the process exits: pending output is written, and
/// unread input given back to seekable files, so that a process sharing the
/// file continues from where this one stopped reading.
pub fn exit_flush() {
    for_each(|inner| {
        let _ = inner.flush();
    });
}
