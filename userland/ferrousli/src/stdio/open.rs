//! Opening and closing streams: `fopen`, `fdopen`, `freopen`, `fclose`,
//! `fileno`, `tmpfile`, and `remove`.

use core::ffi::{c_char, c_int};
use core::ptr::null_mut;

use super::EOF;
use super::file::{self, Backend, File, Inner, Mode};
use super::sys;
use crate::errno;

/// A mode string, decoded.
#[derive(Debug, Clone, Copy)]
pub struct OpenMode {
    /// `r`, `w` or `a`.
    pub first: u8,
    /// The flags for `open`.
    pub flags: c_int,
    /// Whether the stream may be read.
    pub readable: bool,
    /// Whether the stream may be written.
    pub writable: bool,
    /// Whether writes go to the end.
    pub append: bool,
}

/// Decodes a mode string: `r`, `w` or `a`, then any of `+` (read and write),
/// `b` (ignored), `x` (fail if the file exists) and `e` (close on exec), in
/// any order. Other characters are ignored, as glibc and musl ignore them.
/// `None` if the first character is not `r`, `w` or `a`.
///
/// # Safety
///
/// `mode` must be a NUL-terminated string.
pub unsafe fn parse_mode(mode: *const c_char) -> Option<OpenMode> {
    // SAFETY: the caller passes a NUL-terminated string.
    let first = unsafe { mode.read() } as u8;
    let mut parsed = match first {
        b'r' => OpenMode {
            first,
            flags: sys::O_RDONLY,
            readable: true,
            writable: false,
            append: false,
        },
        b'w' => OpenMode {
            first,
            flags: sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC,
            readable: false,
            writable: true,
            append: false,
        },
        b'a' => OpenMode {
            first,
            flags: sys::O_WRONLY | sys::O_CREAT | sys::O_APPEND,
            readable: false,
            writable: true,
            append: true,
        },
        _ => return None,
    };
    let mut at = mode.wrapping_add(1);
    loop {
        // SAFETY: no NUL came before `at`, so it is inside the string.
        match unsafe { at.read() } as u8 {
            0 => break,
            b'+' => {
                parsed.flags = (parsed.flags & !sys::O_ACCMODE) | sys::O_RDWR;
                parsed.readable = true;
                parsed.writable = true;
            }
            b'x' => parsed.flags |= sys::O_EXCL,
            b'e' => parsed.flags |= sys::O_CLOEXEC,
            _ => {}
        }
        at = at.wrapping_add(1);
    }
    Some(parsed)
}

/// A new stream on the descriptor `fd`. Null if there is no memory.
fn descriptor_stream(fd: c_int, mode: &OpenMode) -> *mut File {
    let mut inner = Inner::new(Backend::Fd(fd), mode.readable, mode.writable, Mode::Auto);
    inner.append = mode.append;
    file::allocate(inner)
}

/// Opens the file `path` as a stream.
///
/// # Safety
///
/// `path` and `mode` must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut File {
    // SAFETY: the caller passes a NUL-terminated mode.
    let Some(mode) = (unsafe { parse_mode(mode) }) else {
        errno::set(errno::EINVAL);
        return null_mut();
    };
    // SAFETY: the caller passes a NUL-terminated path.
    let fd = match unsafe { sys::open(path, mode.flags, 0o666) } {
        Ok(fd) => fd,
        Err(error) => {
            errno::set(error);
            return null_mut();
        }
    };
    let file = descriptor_stream(fd, &mode);
    if file.is_null() {
        let _ = sys::close(fd);
    }
    file
}

/// Opens a stream on the open descriptor `fd`. Fails with `EBADF` if `fd` is
/// not open, and with `EINVAL` if its access mode does not allow what `mode`
/// asks. Mode `a` turns on `O_APPEND`, and `e` close-on-exec.
///
/// # Safety
///
/// `mode` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fdopen(fd: c_int, mode: *const c_char) -> *mut File {
    // SAFETY: the caller passes a NUL-terminated mode.
    let Some(mode) = (unsafe { parse_mode(mode) }) else {
        errno::set(errno::EINVAL);
        return null_mut();
    };
    let status = match sys::fcntl(fd, sys::F_GETFL, 0) {
        Ok(status) => status,
        Err(error) => {
            errno::set(error);
            return null_mut();
        }
    };
    let access = status & sys::O_ACCMODE;
    if (mode.readable && access == sys::O_WRONLY) || (mode.writable && access == sys::O_RDONLY) {
        errno::set(errno::EINVAL);
        return null_mut();
    }
    if mode.flags & sys::O_CLOEXEC != 0 {
        let _ = sys::fcntl(fd, sys::F_SETFD, sys::FD_CLOEXEC);
    }
    if mode.append && status & sys::O_APPEND == 0 {
        let _ = sys::fcntl(fd, sys::F_SETFL, status | sys::O_APPEND);
    }
    descriptor_stream(fd, &mode)
}

/// Reopens `stream` on `path` with `mode`, or with a null `path` changes the
/// mode of its descriptor. The stream keeps its descriptor number: the new
/// file is duplicated onto it, so `freopen` on `stdout` redirects descriptor
/// 1. On failure the stream is closed and null returned.
///
/// # Safety
///
/// `path` must be null or a NUL-terminated string, `mode` a NUL-terminated
/// string, and `stream` a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn freopen(
    path: *const c_char,
    mode: *const c_char,
    stream: *mut File,
) -> *mut File {
    // SAFETY: the caller passes a NUL-terminated mode.
    let parsed = unsafe { parse_mode(mode) };
    let standard = [&file::STDIN_FILE, &file::STDOUT_FILE, &file::STDERR_FILE]
        .iter()
        .position(|each| core::ptr::eq(*each, stream))
        .map(|index| index as c_int);
    let op = |inner: &mut Inner| {
        let Some(mode) = parsed else {
            errno::set(errno::EINVAL);
            return false;
        };
        let _ = inner.flush();
        // SAFETY: the caller passes a null or NUL-terminated path.
        let result = unsafe { reopen(inner, path, &mode, standard) };
        if let Err(error) = result {
            errno::set(error);
            return false;
        }
        inner.reset(mode.readable, mode.writable, mode.append);
        true
    };
    // SAFETY: the caller passes a live stream.
    let reopened = unsafe { file::locked(stream, op) };
    if reopened {
        return stream;
    }
    // SAFETY: as above.
    let _ = unsafe { file::locked(stream, Inner::close) };
    // SAFETY: the stream is closed, and `freopen`'s failure ends its use.
    unsafe { file::free(stream) };
    null_mut()
}

/// The descriptor work of `freopen`.
///
/// # Safety
///
/// `path` must be null or a NUL-terminated string.
unsafe fn reopen(
    inner: &mut Inner,
    path: *const c_char,
    mode: &OpenMode,
    standard: Option<c_int>,
) -> Result<(), c_int> {
    if path.is_null() {
        let Backend::Fd(fd) = inner.backend else {
            return Err(errno::EBADF);
        };
        if mode.flags & sys::O_CLOEXEC != 0 {
            let _ = sys::fcntl(fd, sys::F_SETFD, sys::FD_CLOEXEC);
        }
        let status = mode.flags & !(sys::O_CREAT | sys::O_EXCL | sys::O_CLOEXEC | sys::O_TRUNC);
        return sys::fcntl(fd, sys::F_SETFL, status).map(|_| ());
    }
    // SAFETY: the caller passes a NUL-terminated path.
    let fd = unsafe { sys::open(path, mode.flags, 0o666) }?;
    let keep = match inner.backend {
        Backend::Fd(old) if old >= 0 => Some(old),
        Backend::Closed => standard,
        _ => None,
    };
    match keep {
        Some(old) if old == fd => {}
        Some(old) => {
            if let Err(error) = sys::dup3(fd, old, mode.flags & sys::O_CLOEXEC) {
                let _ = sys::close(fd);
                return Err(error);
            }
            let _ = sys::close(fd);
            inner.backend = Backend::Fd(old);
        }
        None => {
            let _ = inner.close();
            inner.backend = Backend::Fd(fd);
        }
    }
    Ok(())
}

/// Flushes and closes `stream`, and frees it. Returns `EOF` if the flush or
/// the close failed; the stream is gone either way.
///
/// # Safety
///
/// `stream` must be a live stream, not used again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fclose(stream: *mut File) -> c_int {
    // SAFETY: the caller passes a live stream.
    let closed = unsafe { file::locked(stream, Inner::close) };
    // SAFETY: the stream is closed and the caller will not use it again.
    unsafe { file::free(stream) };
    if closed { 0 } else { EOF }
}

/// The descriptor of `inner`, or -1 with `EBADF`.
fn descriptor(inner: &mut Inner) -> c_int {
    match inner.backend {
        Backend::Fd(fd) if fd >= 0 => fd,
        _ => {
            errno::set(errno::EBADF);
            -1
        }
    }
}

/// The descriptor a stream reads and writes, or -1 with `EBADF` for a memory
/// or cookie stream.
///
/// # Safety
///
/// `stream` must be a live stream.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fileno(stream: *mut File) -> c_int {
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, descriptor) }
}

/// `fileno` without the lock.
///
/// # Safety
///
/// `stream` must be a live stream whose lock the caller holds or need not.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fileno_unlocked(stream: *mut File) -> c_int {
    // SAFETY: the caller passes a live stream and vouches for its lock.
    unsafe { file::unlocked(stream, descriptor) }
}

/// Opens a new, nameless file in `/tmp` for reading and writing, which
/// disappears when it is closed.
///
/// It is made with `O_TMPFILE`, so it never has a name. A file system without
/// `O_TMPFILE` gets a file with a random name, created exclusively and removed
/// at once, as musl does.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tmpfile() -> *mut File {
    let mode = OpenMode {
        first: b'w',
        flags: sys::O_RDWR,
        readable: true,
        writable: true,
        append: false,
    };
    // SAFETY: the path is a C string literal.
    let fd = match unsafe { sys::open(c"/tmp".as_ptr(), sys::O_TMPFILE | sys::O_RDWR, 0o600) } {
        Ok(fd) => fd,
        Err(_) => match named_temporary() {
            Ok(fd) => fd,
            Err(error) => {
                errno::set(error);
                return null_mut();
            }
        },
    };
    let file = descriptor_stream(fd, &mode);
    if file.is_null() {
        let _ = sys::close(fd);
    }
    file
}

/// Creates a file with a random name in `/tmp`, and removes the name.
fn named_temporary() -> Result<c_int, c_int> {
    const ALPHABET: &[u8; 64] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
    let mut last = errno::EEXIST;
    for _ in 0..100 {
        let mut name = *b"/tmp/tmpfile_XXXXXX\0";
        let mut random = [0_u8; 6];
        sys::random(&mut random)?;
        for (slot, byte) in name.iter_mut().skip(13).zip(random) {
            *slot = ALPHABET
                .get(usize::from(byte & 63))
                .copied()
                .unwrap_or(b'x');
        }
        let flags = sys::O_RDWR | sys::O_CREAT | sys::O_EXCL;
        // SAFETY: `name` is NUL-terminated.
        match unsafe { sys::open(name.as_ptr().cast(), flags, 0o600) } {
            Ok(fd) => {
                // SAFETY: as above.
                let _ = unsafe { sys::unlink(name.as_ptr().cast(), 0) };
                return Ok(fd);
            }
            Err(error) if error == errno::EEXIST => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

/// Removes the file or empty directory `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn remove(path: *const c_char) -> c_int {
    // SAFETY: the caller passes a NUL-terminated path.
    let mut result = unsafe { sys::unlink(path, 0) };
    if result == Err(errno::EISDIR) {
        // SAFETY: as above.
        result = unsafe { sys::unlink(path, sys::AT_REMOVEDIR) };
    }
    match result {
        Ok(()) => 0,
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// glibc's large-file name for [`fopen`]; every stream here is large-file.
///
/// # Safety
///
/// As [`fopen`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fopen64(path: *const c_char, mode: *const c_char) -> *mut File {
    // SAFETY: the caller's contract is `fopen`'s.
    unsafe { fopen(path, mode) }
}

/// glibc's large-file name for [`freopen`].
///
/// # Safety
///
/// As [`freopen`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn freopen64(
    path: *const c_char,
    mode: *const c_char,
    stream: *mut File,
) -> *mut File {
    // SAFETY: the caller's contract is `freopen`'s.
    unsafe { freopen(path, mode, stream) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(text: &core::ffi::CStr) -> Option<OpenMode> {
        // SAFETY: a C string literal.
        unsafe { parse_mode(text.as_ptr()) }
    }

    #[test]
    fn mode_strings_decode_to_open_flags() {
        let read = mode(c"rb").map(|m| (m.flags, m.readable, m.writable));
        assert_eq!(read, Some((sys::O_RDONLY, true, false)));
        let write = mode(c"wx").map(|m| m.flags);
        assert_eq!(
            write,
            Some(sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_EXCL)
        );
        let append = mode(c"a+e").map(|m| (m.flags, m.readable, m.append));
        assert_eq!(
            append,
            Some((
                sys::O_RDWR | sys::O_CREAT | sys::O_APPEND | sys::O_CLOEXEC,
                true,
                true
            ))
        );
        let update = mode(c"r+b").map(|m| (m.flags, m.writable));
        assert_eq!(update, Some((sys::O_RDWR, true)));
        assert!(mode(c"x").is_none());
        assert!(mode(c"").is_none());
    }
}
