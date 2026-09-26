//! `pwd.h`: the user database in `/etc/passwd`; `unistd.h`'s `getlogin` and
//! `getlogin_r`; and the legacy `getusershell`, `setusershell` and
//! `endusershell` over `/etc/shells`.
//!
//! The parsing is musl's `passwd/getpwent_a.c`: seven fields separated by
//! colons, the ids read as decimal digits, and a line whose fields do not all
//! parse skipped. musl asks nscd when the file has no answer; there is no nscd
//! here, so the file is the whole database, and a missing file answers every
//! lookup with nothing. Three things differ from musl:
//!
//! * Only a trailing newline is removed from a line. musl drops each line's
//!   last byte, so a last line without a newline loses a character.
//! * `getpwnam_r` and `getpwuid_r` need room for the line, not for the whole
//!   buffer `getline` happened to allocate for it.
//! * `getusershell` skips blank lines and comments, as glibc does. musl returns
//!   them as shells.
//!
//! `getpwent`, `getpwnam` and `getpwuid` return static storage that the next
//! call to any of them overwrites, and like the other non-reentrant functions
//! here are not safe to call from two threads at once, as in C.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_int, c_uint};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::errno;
use crate::malloc::{free, realloc};
use crate::stdio::file::File;
use crate::stdio::io::{ferror, getline};
use crate::stdio::open::{fclose, fopen};
use crate::stdlib::getenv;
use crate::string::{strcmp, strlen};

/// C's `struct passwd`, from `include/pwd.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Passwd {
    /// The user name.
    pub pw_name: *mut c_char,
    /// The password field, usually `x`.
    pub pw_passwd: *mut c_char,
    /// The user id.
    pub pw_uid: c_uint,
    /// The primary group id.
    pub pw_gid: c_uint,
    /// The comment, usually the full name.
    pub pw_gecos: *mut c_char,
    /// The home directory.
    pub pw_dir: *mut c_char,
    /// The login shell.
    pub pw_shell: *mut c_char,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Passwd>() == 48);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Passwd, pw_uid) == 8);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Passwd, pw_gecos) == 16);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Passwd>() == 28);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Passwd, pw_uid) == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Passwd, pw_gecos) == 24);

impl Passwd {
    /// An entry with every string null.
    const EMPTY: Self = Self {
        pw_name: null_mut(),
        pw_passwd: null_mut(),
        pw_uid: 0,
        pw_gid: 0,
        pw_gecos: null_mut(),
        pw_dir: null_mut(),
        pw_shell: null_mut(),
    };
}

/// State one of these databases keeps between calls.
#[derive(Debug)]
pub(crate) struct Shared<T>(pub(crate) UnsafeCell<T>);

// SAFETY: C documents the functions using this state as unsafe to call from
// two threads at once, and each of them reaches it only for the length of one
// call.
unsafe impl<T> Sync for Shared<T> {}

/// The calling thread's `errno`.
pub(crate) fn last_errno() -> c_int {
    // SAFETY: the pointer is this thread's errno.
    unsafe { errno::__errno_location().read() }
}

/// A line buffer from `getline`, kept between reads.
#[derive(Debug)]
pub(crate) struct Line {
    /// The buffer, or null before the first read.
    pub(crate) ptr: *mut c_char,
    /// The buffer's size.
    pub(crate) cap: usize,
}

impl Line {
    /// No buffer yet.
    pub(crate) const EMPTY: Self = Self {
        ptr: null_mut(),
        cap: 0,
    };

    /// Reads the next line of `stream`, removes its newline, and returns its
    /// length, `Ok(None)` at the end, or the error.
    ///
    /// # Safety
    ///
    /// `stream` must be a live stream.
    pub(crate) unsafe fn read(&mut self, stream: *mut File) -> Result<Option<usize>, c_int> {
        // SAFETY: the buffer is null or an earlier `getline`'s of `cap` bytes,
        // and the caller passes a live stream.
        let got = unsafe { getline(&raw mut self.ptr, &raw mut self.cap, stream) };
        let Ok(len) = usize::try_from(got) else {
            // SAFETY: as above.
            return if unsafe { ferror(stream) } != 0 {
                Err(last_errno())
            } else {
                Ok(None)
            };
        };
        // SAFETY: `getline` left `len` bytes and a NUL.
        let bytes = unsafe { self.bytes(len) };
        if let Some(newline @ b'\n') = len.checked_sub(1).and_then(|last| bytes.get_mut(last)) {
            *newline = 0;
            return Ok(Some(len - 1));
        }
        Ok(Some(len))
    }

    /// The line's `len` bytes and the byte after them, for a parser to write
    /// NULs into.
    ///
    /// # Safety
    ///
    /// The buffer must hold `len + 1` bytes, and nothing else refer into it
    /// while the slice lives.
    pub(crate) unsafe fn bytes(&mut self, len: usize) -> &mut [u8] {
        // SAFETY: the caller vouches for the bytes.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.cast::<u8>(), len + 1) }
    }

    /// Frees the buffer.
    pub(crate) fn release(&mut self) {
        // SAFETY: the buffer is null or from `getline`, which allocates with
        // `malloc`.
        unsafe { free(self.ptr.cast()) };
        *self = Self::EMPTY;
    }
}

/// The first `:` at or after `from`, before the NUL that ends `line`.
pub(crate) fn colon(line: &[u8], from: usize) -> Option<usize> {
    let offset = line
        .get(from..)?
        .iter()
        .take_while(|&&byte| byte != 0)
        .position(|&byte| byte == b':')?;
    Some(from + offset)
}

/// Reads decimal digits from `at`, wrapping as C's unsigned arithmetic does,
/// and returns the value and where the digits end.
pub(crate) fn digits(line: &[u8], mut at: usize) -> (c_uint, usize) {
    let mut value: c_uint = 0;
    while let Some(&digit @ b'0'..=b'9') = line.get(at) {
        value = value
            .wrapping_mul(10)
            .wrapping_add(c_uint::from(digit - b'0'));
        at += 1;
    }
    (value, at)
}

/// Ends a field at `at` with a NUL.
pub(crate) fn cut(line: &mut [u8], at: usize) {
    if let Some(byte) = line.get_mut(at) {
        *byte = 0;
    }
}

/// Copies `len` bytes from `from` to `to`.
///
/// # Safety
///
/// `from` must be valid for reads and `to` for writes of `len` bytes, and they
/// must not overlap.
pub(crate) unsafe fn copy(from: *const c_char, to: *mut c_char, len: usize) {
    for offset in 0..len {
        // SAFETY: the caller vouches for `len` readable bytes.
        let byte = unsafe { from.wrapping_add(offset).read() };
        // SAFETY: the caller vouches for `len` writable bytes.
        unsafe { to.wrapping_add(offset).write(byte) };
    }
}

/// Opens the database file `path` for reading. `Ok(None)` if it does not
/// exist, which answers every lookup with nothing, as musl's does once nscd
/// has no answer either.
pub(crate) fn open_database(path: &CStr) -> Result<Option<*mut File>, c_int> {
    // SAFETY: both strings are NUL-terminated.
    let stream = unsafe { fopen(path.as_ptr(), c"rbe".as_ptr()) };
    if !stream.is_null() {
        return Ok(Some(stream));
    }
    match last_errno() {
        errno::ENOENT | errno::ENOTDIR => Ok(None),
        error => Err(error),
    }
}

/// Where a `/etc/passwd` line's fields are.
#[derive(Debug, PartialEq, Eq)]
struct Fields {
    /// Where the password field starts.
    passwd: usize,
    /// The user id.
    uid: c_uint,
    /// The group id.
    gid: c_uint,
    /// Where the comment starts.
    gecos: usize,
    /// Where the home directory starts.
    dir: usize,
    /// Where the shell starts.
    shell: usize,
}

/// Splits `line`, which ends in a NUL, into its fields with NULs, as musl's
/// `__getpwent_a` does, or `None` if one is missing. The name starts the line,
/// and the colon after it is looked for from the second byte.
fn parse(line: &mut [u8]) -> Option<Fields> {
    let end_name = colon(line, 1)?;
    cut(line, end_name);
    let passwd = end_name + 1;
    let end_passwd = colon(line, passwd)?;
    cut(line, end_passwd);
    let (uid, end_uid) = digits(line, end_passwd + 1);
    if line.get(end_uid) != Some(&b':') {
        return None;
    }
    cut(line, end_uid);
    let (gid, end_gid) = digits(line, end_uid + 1);
    if line.get(end_gid) != Some(&b':') {
        return None;
    }
    cut(line, end_gid);
    let gecos = end_gid + 1;
    let end_gecos = colon(line, gecos)?;
    cut(line, end_gecos);
    let dir = end_gecos + 1;
    let end_dir = colon(line, dir)?;
    cut(line, end_dir);
    Some(Fields {
        passwd,
        uid,
        gid,
        gecos,
        dir,
        shell: end_dir + 1,
    })
}

/// Reads the next entry of `stream` into `line`, skipping lines that do not
/// parse, and returns it with the line's length.
///
/// # Safety
///
/// `stream` must be a live stream.
unsafe fn next(stream: *mut File, line: &mut Line) -> Result<Option<(Passwd, usize)>, c_int> {
    loop {
        // SAFETY: the caller passes a live stream.
        let Some(len) = (unsafe { line.read(stream) })? else {
            return Ok(None);
        };
        let base = line.ptr;
        // SAFETY: the line was just read: `len` bytes and a NUL.
        let Some(fields) = parse(unsafe { line.bytes(len) }) else {
            continue;
        };
        let entry = Passwd {
            pw_name: base,
            pw_passwd: base.wrapping_add(fields.passwd),
            pw_uid: fields.uid,
            pw_gid: fields.gid,
            pw_gecos: base.wrapping_add(fields.gecos),
            pw_dir: base.wrapping_add(fields.dir),
            pw_shell: base.wrapping_add(fields.shell),
        };
        return Ok(Some((entry, len)));
    }
}

/// Finds the entry named `name`, or with id `uid` if `name` is null, reading
/// into `line`.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe fn find(
    name: *const c_char,
    uid: c_uint,
    line: &mut Line,
) -> Result<Option<(Passwd, usize)>, c_int> {
    let Some(stream) = open_database(c"/etc/passwd")? else {
        return Ok(None);
    };
    let found = loop {
        // SAFETY: `stream` is the file just opened.
        match unsafe { next(stream, line) } {
            Ok(Some((entry, len))) => {
                let hit = if name.is_null() {
                    entry.pw_uid == uid
                } else {
                    // SAFETY: both are NUL-terminated strings.
                    unsafe { strcmp(name, entry.pw_name) == 0 }
                };
                if hit {
                    break Ok(Some((entry, len)));
                }
            }
            other => break other,
        }
    };
    // SAFETY: `stream` is live, and not used again.
    let _ = unsafe { fclose(stream) };
    found
}

/// What `getpwent`, `getpwnam` and `getpwuid` keep.
#[derive(Debug)]
struct State {
    /// The file `getpwent` reads, or null.
    stream: *mut File,
    /// The line the last entry's strings are in.
    line: Line,
    /// The entry returned.
    entry: Passwd,
}

/// The state of `getpwent`, `getpwnam` and `getpwuid`.
static STATE: Shared<State> = Shared(UnsafeCell::new(State {
    stream: null_mut(),
    line: Line::EMPTY,
    entry: Passwd::EMPTY,
}));

/// The state, for the length of one call.
fn state() -> &'static mut State {
    // SAFETY: see `Shared`.
    unsafe { &mut *STATE.0.get() }
}

/// Stores `found` as the entry to return, or reports what went wrong.
fn answer(found: Result<Option<(Passwd, usize)>, c_int>) -> *mut Passwd {
    let state = state();
    match found {
        Ok(Some((entry, _))) => {
            state.entry = entry;
            &raw mut state.entry
        }
        Ok(None) => null_mut(),
        Err(error) => {
            errno::set(error);
            null_mut()
        }
    }
}

/// The next entry of the user database, in static storage, or null at the end
/// or with `errno` set.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getpwent() -> *mut Passwd {
    let state = state();
    if state.stream.is_null() {
        match open_database(c"/etc/passwd") {
            Ok(Some(stream)) => state.stream = stream,
            Ok(None) => return null_mut(),
            Err(error) => {
                errno::set(error);
                return null_mut();
            }
        }
    }
    // SAFETY: the stream is the file opened above or by an earlier call.
    let found = unsafe { next(state.stream, &mut state.line) };
    answer(found)
}

/// Starts `getpwent` again from the first entry.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setpwent() {
    let state = state();
    if !state.stream.is_null() {
        // SAFETY: the stream is live, and forgotten here.
        let _ = unsafe { fclose(state.stream) };
        state.stream = null_mut();
    }
}

/// Closes the file `getpwent` reads. It is `setpwent`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endpwent() {
    setpwent();
}

/// The entry for user `name`, in static storage, or null if there is none.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getpwnam(name: *const c_char) -> *mut Passwd {
    // SAFETY: the caller passes a NUL-terminated name.
    let found = unsafe { find(name, 0, &mut state().line) };
    answer(found)
}

/// The entry for user id `uid`, in static storage, or null if there is none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getpwuid(uid: c_uint) -> *mut Passwd {
    // SAFETY: there is no name.
    let found = unsafe { find(null_mut(), uid, &mut state().line) };
    answer(found)
}

/// The reentrant lookup: the entry goes in `*pw`, its strings in the `size`
/// bytes at `buf`, and `*result` is `pw`, or null if there is none. Returns 0
/// or the error, `ERANGE` if the strings do not fit.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string, `pw` and `result` valid
/// for writes, and `buf` for writes of `size` bytes.
unsafe fn lookup_r(
    name: *const c_char,
    uid: c_uint,
    pw: *mut Passwd,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Passwd,
) -> c_int {
    // SAFETY: the caller passes a writable result.
    unsafe { result.write(null_mut()) };
    let mut line = Line::EMPTY;
    // SAFETY: the caller passes a valid name.
    let error = match unsafe { find(name, uid, &mut line) } {
        Ok(Some((found, len))) if len < size => {
            // SAFETY: the line holds `len` bytes and a NUL, and `buf` at least
            // that many, in other memory.
            unsafe { copy(line.ptr, buf, len + 1) };
            let moved =
                |field: *mut c_char| buf.wrapping_add(field.addr().wrapping_sub(line.ptr.addr()));
            let entry = Passwd {
                pw_name: moved(found.pw_name),
                pw_passwd: moved(found.pw_passwd),
                pw_gecos: moved(found.pw_gecos),
                pw_dir: moved(found.pw_dir),
                pw_shell: moved(found.pw_shell),
                ..found
            };
            // SAFETY: the caller passes a writable entry.
            unsafe { pw.write(entry) };
            // SAFETY: the caller passes a writable result.
            unsafe { result.write(pw) };
            0
        }
        Ok(Some(_)) => errno::ERANGE,
        Ok(None) => 0,
        Err(error) => error,
    };
    line.release();
    if error != 0 {
        errno::set(error);
    }
    error
}

/// The entry for user `name`, reentrantly: see `lookup_r`.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, `pw` and `result` valid for
/// writes, and `buf` for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getpwnam_r(
    name: *const c_char,
    pw: *mut Passwd,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Passwd,
) -> c_int {
    // SAFETY: the caller's promises are `lookup_r`'s.
    unsafe { lookup_r(name, 0, pw, buf, size, result) }
}

/// The entry for user id `uid`, reentrantly: see `lookup_r`.
///
/// # Safety
///
/// `pw` and `result` must be valid for writes, and `buf` for writes of `size`
/// bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getpwuid_r(
    uid: c_uint,
    pw: *mut Passwd,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Passwd,
) -> c_int {
    // SAFETY: the caller's promises are `lookup_r`'s.
    unsafe { lookup_r(null_mut(), uid, pw, buf, size, result) }
}

/// The name of the user logged in, from `LOGNAME`, as musl does, or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getlogin() -> *mut c_char {
    // SAFETY: the name is NUL-terminated.
    unsafe { getenv(c"LOGNAME".as_ptr()) }
}

/// Stores `getlogin`'s name in the `size` bytes at `name`. Returns 0, `ENXIO`
/// if there is no name, or `ERANGE` if it and its NUL do not fit.
///
/// # Safety
///
/// `name` must be valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getlogin_r(name: *mut c_char, size: usize) -> c_int {
    let login = getlogin();
    if login.is_null() {
        return errno::ENXIO;
    }
    // SAFETY: the environment's strings are NUL-terminated.
    let len = unsafe { strlen(login) };
    if len >= size {
        return errno::ERANGE;
    }
    // SAFETY: `len + 1` bytes are readable at `login` and writable at `name`.
    unsafe { copy(login, name, len + 1) };
    0
}

/// The shells `getusershell` lists when `/etc/shells` cannot be read, musl's.
const DEFAULT_SHELLS: [&[u8]; 2] = [b"/bin/sh", b"/bin/csh"];

/// What `getusershell` keeps.
#[derive(Debug)]
struct Shells {
    /// `/etc/shells`, or null.
    stream: *mut File,
    /// Without the file, the index of the next default shell.
    defaults: Option<usize>,
    /// The line returned.
    line: Line,
}

/// The state of `getusershell`.
static SHELLS: Shared<Shells> = Shared(UnsafeCell::new(Shells {
    stream: null_mut(),
    defaults: None,
    line: Line::EMPTY,
}));

/// `getusershell`'s state, for the length of one call.
fn shells() -> &'static mut Shells {
    // SAFETY: see `Shared`.
    unsafe { &mut *SHELLS.0.get() }
}

/// Opens `/etc/shells`, or falls back to the defaults, if neither is open.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setusershell() {
    let state = shells();
    if !state.stream.is_null() || state.defaults.is_some() {
        return;
    }
    match open_database(c"/etc/shells") {
        Ok(Some(stream)) => state.stream = stream,
        _ => state.defaults = Some(0),
    }
}

/// Closes `/etc/shells`, so the next `getusershell` starts again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endusershell() {
    let state = shells();
    if !state.stream.is_null() {
        // SAFETY: the stream is live, and forgotten here.
        let _ = unsafe { fclose(state.stream) };
        state.stream = null_mut();
    }
    state.defaults = None;
}

/// Copies `text` and a NUL into `line`, growing it if needed.
fn store(line: &mut Line, text: &[u8]) -> *mut c_char {
    let need = text.len() + 1;
    if line.cap < need {
        // SAFETY: the buffer is null or from `malloc`.
        let grown = unsafe { realloc(line.ptr.cast(), need) };
        if grown.is_null() {
            return null_mut();
        }
        line.ptr = grown.cast();
        line.cap = need;
    }
    for (offset, &byte) in text.iter().chain(&[0]).enumerate() {
        // SAFETY: the buffer holds `need` bytes.
        unsafe { line.ptr.wrapping_add(offset).write(byte as c_char) };
    }
    line.ptr
}

/// The next shell users may have, or null after the last.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getusershell() -> *mut c_char {
    setusershell();
    let state = shells();
    if let Some(next) = state.defaults {
        let Some(shell) = DEFAULT_SHELLS.get(next) else {
            return null_mut();
        };
        state.defaults = Some(next + 1);
        return store(&mut state.line, shell);
    }
    loop {
        // SAFETY: `setusershell` opened the stream.
        let Ok(Some(len)) = (unsafe { state.line.read(state.stream) }) else {
            return null_mut();
        };
        // SAFETY: the line was just read.
        let first = unsafe { state.line.bytes(len) }.first().copied();
        if len > 0 && first != Some(b'#') {
            return state.line.ptr;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(text: &str) -> (Option<Fields>, Vec<u8>) {
        let mut line: Vec<u8> = text.bytes().chain([0]).collect();
        (parse(&mut line), line)
    }

    #[test]
    fn a_line_splits_into_seven_fields() {
        let (found, line) = fields("root:x:0:0:root:/root:/bin/sh");
        let found = found.unwrap_or(Fields {
            passwd: 0,
            uid: 9,
            gid: 9,
            gecos: 0,
            dir: 0,
            shell: 0,
        });
        assert_eq!((found.uid, found.gid), (0, 0));
        assert_eq!(line.get(found.dir..found.dir + 5), Some(&b"/root"[..]));
        assert_eq!(line.get(found.shell..), Some(&b"/bin/sh\0"[..]));
        assert_eq!(line.get(4), Some(&0));
    }

    #[test]
    fn empty_fields_parse_and_missing_ones_do_not() {
        let (found, _) = fields("nobody:::4294967295::/:");
        assert!(found.is_some_and(|found| found.uid == 0 && found.gid == u32::MAX));
        assert_eq!(fields("a:x:1:2:g:/h").0, None);
        assert_eq!(fields("a:x:1x:2:g:/h:/s").0, None);
        assert_eq!(fields("").0, None);
        assert_eq!(fields("# comment").0, None);
    }
}
