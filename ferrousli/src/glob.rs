//! `glob.h`: the path names that match a shell pattern.
//!
//! Adapted from musl 1.2.5 (MIT), `src/regex/glob.c`. The pattern is walked a
//! component at a time. The longest literal prefix, with escapes removed, is
//! copied into a path buffer without reading any directory; a component with a
//! wildcard is matched with [`fnmatch`](crate::fnmatch) against each entry of
//! the directory so far, and the rest of the pattern is globbed below each
//! entry that matches. A path with no wildcard left is checked with `lstat`,
//! or with `stat` when `GLOB_MARK` needs to know whether a link names a
//! directory. The entries' `d_type` lets directories be told from files
//! without a `stat` where the file system reports it.
//!
//! As in musl:
//!
//! * a directory that cannot be opened or read is reported to the error
//!   callback, `ENOENT` included, and aborts the search if the callback
//!   returns nonzero or `GLOB_ERR` is set;
//! * without `GLOB_PERIOD` a leading `.` must be matched literally, and with
//!   it `.` and `..` still match only a pattern that names them, except in the
//!   last component;
//! * `GLOB_TILDE` and `GLOB_TILDE_CHECK` both expand `~` to `$HOME`, or the
//!   user's home directory, and `~name` to that user's, and both fail with
//!   `GLOB_NOMATCH` for an unknown user;
//! * paths longer than `PATH_MAX` are skipped.
//!
//! There are no name service switch modules here, so a home directory comes
//! from `/etc/passwd`, which is where musl's `getpwnam_r` reads first.
//!
//! Each path in `gl_pathv` is its own allocation from `malloc`, which
//! `globfree` frees. `glob_t` in musl's header is 72 bytes, the size of
//! glibc's, whose `gl_flags` and five `GLOB_ALTDIRFUNC` function pointers sit
//! where musl has padding; `GLOB_ALTDIRFUNC` is not supported. `glob64` and
//! `globfree64`, glibc's names, are the same functions.

use core::ffi::{CStr, c_char, c_int, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::dirent::{Dirent, S_IFDIR, S_IFMT, closedir, close_quietly, get_errno, opendir, readdir};
use crate::errno;
use crate::fcntl::AT_FDCWD;
use crate::fnmatch::{self, FNM_NOESCAPE, FNM_PERIOD};
use crate::growable::{Growable, sort_by};
use crate::malloc::{free, malloc, realloc};
use crate::stat::{Stat, lstat, stat};
use crate::stdlib::getenv;
use crate::string::{memcpy, strcmp};
use crate::syscall::{self, nr};

/// `glob_t`, as musl's `glob.h` lays it out.
#[repr(C)]
#[derive(Debug)]
pub struct Glob {
    /// How many paths matched.
    pub gl_pathc: usize,
    /// The paths, after `gl_offs` null slots, ending with a null.
    pub gl_pathv: *mut *mut c_char,
    /// How many null slots `GLOB_DOOFFS` reserves.
    pub gl_offs: usize,
    /// Unused, as in musl.
    dummy1: c_int,
    /// Unused, as in musl.
    dummy2: [*mut c_void; 5],
}

const _: () = assert!(size_of::<Glob>() == 72);
const _: () = assert!(offset_of!(Glob, gl_pathv) == 8);
const _: () = assert!(offset_of!(Glob, gl_offs) == 16);

/// `GLOB_ERR`: stop at a directory that cannot be read.
const GLOB_ERR: c_int = 0x01;
/// `GLOB_MARK`: end each directory's path with `/`.
const GLOB_MARK: c_int = 0x02;
/// `GLOB_NOSORT`: leave the paths in the order found.
const GLOB_NOSORT: c_int = 0x04;
/// `GLOB_DOOFFS`: reserve `gl_offs` null slots.
const GLOB_DOOFFS: c_int = 0x08;
/// `GLOB_NOCHECK`: return the pattern itself when nothing matches.
const GLOB_NOCHECK: c_int = 0x10;
/// `GLOB_APPEND`: add to the paths of an earlier call.
const GLOB_APPEND: c_int = 0x20;
/// `GLOB_NOESCAPE`: a backslash is an ordinary character.
const GLOB_NOESCAPE: c_int = 0x40;
/// `GLOB_PERIOD`: wildcards may match a leading `.`.
const GLOB_PERIOD: c_int = 0x80;
/// `GLOB_TILDE`: expand a leading `~`.
const GLOB_TILDE: c_int = 0x1000;
/// `GLOB_TILDE_CHECK`: expand a leading `~`, failing for an unknown user.
const GLOB_TILDE_CHECK: c_int = 0x4000;

/// `GLOB_NOSPACE`: memory ran out.
const GLOB_NOSPACE: c_int = 1;
/// `GLOB_ABORTED`: a read error stopped the search.
const GLOB_ABORTED: c_int = 2;
/// `GLOB_NOMATCH`: nothing matched.
const GLOB_NOMATCH: c_int = 3;

/// `PATH_MAX`, from `include/limits.h`.
const PATH_MAX: usize = 4096;

/// `DT_DIR`, from `include/dirent.h`.
const DT_DIR: u8 = 4;
/// `DT_REG`, from `include/dirent.h`.
const DT_REG: u8 = 8;
/// `DT_LNK`, from `include/dirent.h`.
const DT_LNK: u8 = 10;

/// `O_CLOEXEC`, from `asm-generic/fcntl.h`; `O_RDONLY` is 0.
const O_RDONLY_CLOEXEC: usize = 0o2_000_000;

/// An error callback from the program.
type ErrFunc = Option<unsafe extern "C" fn(*const c_char, c_int) -> c_int>;

/// The path built so far, NUL-terminated where it ends.
type PathBuf = [u8; PATH_MAX];

/// `s` from `from` on, empty if `from` is past the end.
fn tail(s: &[u8], from: usize) -> &[u8] {
    s.get(from..).unwrap_or(&[])
}

/// `s[from..to]`, empty if that is not within `s`.
fn span(s: &[u8], from: usize, to: usize) -> &[u8] {
    s.get(from..to).unwrap_or(&[])
}

/// The byte at `i`, or 0 past the end.
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// Stores `byte` at `i` in the path, if it is within it.
fn put(buf: &mut PathBuf, i: usize, byte: u8) {
    if let Some(slot) = buf.get_mut(i) {
        *slot = byte;
    }
}

/// One call's search: its flags, callback and the paths found.
struct Search {
    /// The call's flags.
    flags: c_int,
    /// The program's error callback.
    errfunc: ErrFunc,
    /// Each path found, from `malloc`.
    found: Growable<*mut c_char>,
}

impl Search {
    /// Reports `error` reading the path in `buf`. Returns whether to abort.
    fn report(&self, buf: &PathBuf, error: c_int) -> bool {
        let stop = match self.errfunc {
            Some(f) => {
                // SAFETY: the path is NUL-terminated, and the program's
                // callback accepts a path and an error number.
                let ret = unsafe { f(buf.as_ptr().cast(), error) };
                ret != 0
            }
            None => false,
        };
        stop || self.flags & GLOB_ERR != 0
    }

    /// Adds a copy of `name`, with a `/` after it if `mark` and it has none.
    /// Returns `false` if there is no memory.
    fn append(&mut self, name: &[u8], mark: bool) -> bool {
        let len = name.len();
        let Some(size) = len.checked_add(2) else {
            return false;
        };
        let copy = malloc(size).cast::<u8>();
        if copy.is_null() {
            return false;
        }
        // SAFETY: `copy` holds `len + 2` bytes, and `name` is separate.
        let _ = unsafe { memcpy(copy.cast(), name.as_ptr().cast(), len) };
        let mut end = len;
        if mark && name.last().is_some_and(|&b| b != b'/') {
            // SAFETY: as above.
            unsafe { copy.wrapping_add(end).write(b'/') };
            end += 1;
        }
        // SAFETY: as above.
        unsafe { copy.wrapping_add(end).write(0) };
        if !self.found.push(copy.cast()) {
            // SAFETY: the copy came from `malloc` and is not kept.
            unsafe { free(copy.cast()) };
            return false;
        }
        true
    }

    /// Frees every path found.
    fn free_found(&mut self) {
        for &path in self.found.as_slice() {
            // SAFETY: each path came from `malloc` and is not used again.
            unsafe { free(path.cast()) };
        }
        self.found = Growable::new();
    }
}

/// Copies the literal prefix of `*pat` into the path at `*pos`, removing
/// escapes, up to the component holding the first wildcard. Whole components
/// are consumed, with their `/`, and `*pat` and `*pos` moved past them; a
/// pattern with no wildcard is consumed entirely. Returns `false` if the
/// pattern cannot match: a trailing backslash, or a path too long.
fn literal_prefix(buf: &mut PathBuf, pos: &mut usize, kind: &mut u8, pat: &mut &[u8], noescape: bool) -> bool {
    let mut i = 0;
    let mut copied = 0;
    let mut in_bracket = false;
    let mut overflow = false;
    loop {
        let mut c = at(pat, i);
        if c == b'*' || c == b'?' || (in_bracket && c == b']') {
            return true;
        }
        if i >= pat.len() {
            if overflow {
                return false;
            }
            *pat = &[];
            *pos += copied;
            return true;
        }
        if c == b'[' {
            in_bracket = true;
        } else if c == b'\\' && !noescape {
            // Inside a bracket a backslash is ordinary, so `\]` closes it.
            if in_bracket && at(pat, i + 1) == b']' {
                return true;
            }
            if i + 1 >= pat.len() {
                return false;
            }
            i += 1;
            c = at(pat, i);
        }
        if c == b'/' {
            if overflow || *pos + copied + 1 >= PATH_MAX {
                return false;
            }
            put(buf, *pos + copied, b'/');
            *pos += copied + 1;
            copied = 0;
            *pat = tail(pat, i + 1);
            i = 0;
            in_bracket = false;
            *kind = 0;
            continue;
        }
        // An unclosed bracket is literal, so an overflow inside one only
        // matters if it turns out to be.
        if *pos + copied + 1 < PATH_MAX {
            put(buf, *pos + copied, c);
            copied += 1;
        } else if in_bracket {
            overflow = true;
        } else {
            return false;
        }
        *kind = 0;
        i += 1;
    }
}

/// Globs `pat` below the path `buf[..pos]`, whose `d_type` is `kind` if known
/// and 0 otherwise. Returns 0, `GLOB_ABORTED` or `GLOB_NOSPACE`.
fn do_glob(buf: &mut PathBuf, mut pos: usize, mut kind: u8, mut pat: &[u8], search: &mut Search) -> c_int {
    // Without GLOB_MARK a path's type does not matter.
    if kind == 0 && search.flags & GLOB_MARK == 0 {
        kind = DT_REG;
    }
    if !pat.is_empty() && kind != DT_DIR {
        kind = 0;
    }
    while pos + 1 < PATH_MAX && pat.first() == Some(&b'/') {
        put(buf, pos, b'/');
        pos += 1;
        pat = tail(pat, 1);
    }
    let noescape = search.flags & GLOB_NOESCAPE != 0;
    if !literal_prefix(buf, &mut pos, &mut kind, &mut pat, noescape) {
        return 0;
    }
    put(buf, pos, 0);
    if pat.is_empty() {
        found_path(buf, pos, kind, search)
    } else {
        match_directory(buf, pos, pat, search)
    }
}

/// Adds the path `buf[..pos]`, which has no wildcards left, if it exists.
fn found_path(buf: &mut PathBuf, pos: usize, mut kind: u8, search: &mut Search) -> c_int {
    let mut st = Stat::default();
    let path = buf.as_ptr().cast::<c_char>();
    // A link's own type says nothing of whether it names a directory, so ask
    // `stat`; if that fails, `lstat` still finds a dangling link.
    if search.flags & GLOB_MARK != 0
        && (kind == 0 || kind == DT_LNK)
        // SAFETY: the path is NUL-terminated at `pos`, and `st` writable.
        && unsafe { stat(path, &raw mut st) } == 0
    {
        kind = if st.st_mode & S_IFMT == S_IFDIR {
            DT_DIR
        } else {
            DT_REG
        };
    }
    // SAFETY: as above.
    if kind == 0 && unsafe { lstat(path, &raw mut st) } != 0 {
        let error = get_errno();
        if error != errno::ENOENT && search.report(buf, error) {
            return GLOB_ABORTED;
        }
        return 0;
    }
    let mark = search.flags & GLOB_MARK != 0 && kind == DT_DIR;
    if !search.append(span(buf, 0, pos), mark) {
        return GLOB_NOSPACE;
    }
    0
}

/// Matches the first component of `pat`, which has a wildcard, against each
/// entry of the directory `buf[..pos]`, globbing the rest below each match.
fn match_directory(buf: &mut PathBuf, pos: usize, pat: &[u8], search: &mut Search) -> c_int {
    let noescape = search.flags & GLOB_NOESCAPE != 0;
    // The component ends at the first `/`, or before the backslash escaping
    // it: the escape stays with the rest, where it is removed.
    let (component, rest) = match pat.iter().position(|&b| b == b'/') {
        None => (pat, None),
        Some(slash) => {
            let backslashes = span(pat, 0, slash).iter().rev().take_while(|&&b| b == b'\\').count();
            let end = if !noescape && backslashes % 2 == 1 {
                slash - 1
            } else {
                slash
            };
            (span(pat, 0, end), Some(tail(pat, end)))
        }
    };
    let dir_path = if pos == 0 {
        c".".as_ptr()
    } else {
        buf.as_ptr().cast::<c_char>()
    };
    // SAFETY: the path is NUL-terminated.
    let dir = unsafe { opendir(dir_path) };
    if dir.is_null() {
        let error = get_errno();
        return if search.report(buf, error) { GLOB_ABORTED } else { 0 };
    }
    let saved = get_errno();
    let mut fnm_flags = 0;
    if noescape {
        fnm_flags |= FNM_NOESCAPE;
    }
    if search.flags & GLOB_PERIOD == 0 {
        fnm_flags |= FNM_PERIOD;
    }
    let result = loop {
        errno::set(0);
        // SAFETY: `dir` is this call's open stream.
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            break 0;
        }
        // SAFETY: `readdir` returned a valid entry, used before the next call.
        let r = unsafe { consider_entry(buf, pos, entry, component, rest, fnm_flags, search) };
        if r != 0 {
            break r;
        }
    };
    let read_error = get_errno();
    // SAFETY: the stream is not used again.
    let _ = unsafe { closedir(dir) };
    if result != 0 {
        return result;
    }
    put(buf, pos, 0);
    if read_error != 0 && search.report(buf, read_error) {
        return GLOB_ABORTED;
    }
    errno::set(saved);
    0
}

/// Globs below `entry` of the directory `buf[..pos]` if its name matches
/// `component`.
///
/// # Safety
///
/// `entry` must be a valid directory entry.
unsafe fn consider_entry(
    buf: &mut PathBuf,
    pos: usize,
    entry: *mut Dirent,
    component: &[u8],
    rest: Option<&[u8]>,
    fnm_flags: c_int,
    search: &mut Search,
) -> c_int {
    // SAFETY: the caller passes a valid entry.
    let d_type = unsafe { (*entry).d_type };
    // With pattern left, only a directory or a link to one can match.
    if rest.is_some() && d_type != 0 && d_type != DT_DIR && d_type != DT_LNK {
        return 0;
    }
    let name_ptr = entry.wrapping_byte_add(offset_of!(Dirent, d_name)).cast::<c_char>();
    // SAFETY: an entry's name is a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(name_ptr) }.to_bytes();
    if name.len() >= PATH_MAX - pos {
        return 0;
    }
    if !fnmatch::matches(component, name, fnm_flags) {
        return 0;
    }
    // With GLOB_PERIOD, `.` and `..` still match only a pattern that would
    // match them without it, unless this is the last component.
    if rest.is_some()
        && search.flags & GLOB_PERIOD != 0
        && (name == b"." || name == b"..")
        && !fnmatch::matches(component, name, fnm_flags | FNM_PERIOD)
    {
        return 0;
    }
    for (i, &b) in name.iter().enumerate() {
        put(buf, pos + i, b);
    }
    put(buf, pos + name.len(), 0);
    do_glob(buf, pos + name.len(), d_type, rest.unwrap_or(&[]), search)
}

/// The value of `$HOME`, if it is set.
fn home_from_environment() -> Option<&'static [u8]> {
    // SAFETY: the name is a C string literal.
    let home = unsafe { getenv(c"HOME".as_ptr()) };
    if home.is_null() {
        return None;
    }
    // SAFETY: an environment value is a NUL-terminated string that lives as
    // long as the environment, which the program does not free under glob.
    Some(unsafe { CStr::from_ptr(home) }.to_bytes())
}

/// Reads all of `/etc/passwd` into `data`. Returns `GLOB_NOMATCH` if it cannot
/// be read, and `GLOB_NOSPACE` if there is no memory.
fn read_passwd(data: &mut Growable<u8>) -> Result<(), c_int> {
    // SAFETY: the kernel reads the path, a C string literal.
    let ret = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            AT_FDCWD as usize,
            c"/etc/passwd".as_ptr().addr(),
            O_RDONLY_CLOEXEC,
            0,
        )
    };
    let fd = errno::decode(ret).map_err(|_| GLOB_NOMATCH)? as c_int;
    let mut chunk = [0u8; 4096];
    let result = loop {
        // SAFETY: the kernel writes at most the chunk's length into it.
        let ret = unsafe { syscall::syscall3(nr::READ, fd as usize, chunk.as_mut_ptr().addr(), chunk.len()) };
        match errno::decode(ret) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if !data.reserve(n) {
                    break Err(GLOB_NOSPACE);
                }
                for &b in span(&chunk, 0, n) {
                    let _ = data.push(b);
                }
            }
            Err(errno::EINTR) => {}
            Err(_) => break Err(GLOB_NOMATCH),
        }
    };
    close_quietly(fd);
    result
}

/// Copies the home directory of the user `name`, or of the calling user if
/// `name` is empty, from `/etc/passwd` into `home`.
fn home_from_passwd(name: &[u8], home: &mut Growable<u8>) -> Result<(), c_int> {
    let mut data = Growable::new();
    read_passwd(&mut data)?;
    // SAFETY: `getuid` reads no memory.
    let uid = unsafe { syscall::syscall0(nr::GETUID) }.cast_unsigned();
    for line in data.as_slice().split(|&b| b == b'\n') {
        let mut fields = line.split(|&b| b == b':');
        let (Some(user), _, Some(id), _, _, Some(dir)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let hit = if name.is_empty() {
            core::str::from_utf8(id).ok().and_then(|id| id.parse::<usize>().ok()) == Some(uid)
        } else {
            user == name
        };
        if hit {
            if !home.reserve(dir.len()) {
                return Err(GLOB_NOSPACE);
            }
            for &b in dir {
                let _ = home.push(b);
            }
            return Ok(());
        }
    }
    Err(GLOB_NOMATCH)
}

/// Expands the `~` or `~name` that starts `pat` into the path. Returns the
/// path's length and the pattern after the `/` that ended the name.
fn expand_tilde<'a>(pat: &'a [u8], buf: &mut PathBuf) -> Result<(usize, &'a [u8]), c_int> {
    let after = tail(pat, 1);
    let name_len = after.iter().position(|&b| b == b'/').unwrap_or(after.len());
    let name = span(after, 0, name_len);
    let slash = name_len < after.len();
    let rest = if slash { tail(after, name_len + 1) } else { &[] };

    let mut record = Growable::new();
    let from_environment = if name.is_empty() {
        home_from_environment()
    } else {
        None
    };
    let home = match from_environment {
        Some(home) => home,
        None => {
            home_from_passwd(name, &mut record)?;
            record.as_slice()
        }
    };
    if home.len() > PATH_MAX - 2 {
        return Err(GLOB_NOMATCH);
    }
    for (i, &b) in home.iter().enumerate() {
        put(buf, i, b);
    }
    let mut pos = home.len();
    if slash {
        put(buf, pos, b'/');
        pos += 1;
    }
    put(buf, pos, 0);
    Ok((pos, rest))
}

/// Finds the paths matching `pattern` and stores them in `*pglob`. Returns 0,
/// `GLOB_NOMATCH`, `GLOB_ABORTED` or `GLOB_NOSPACE`.
///
/// # Safety
///
/// `pattern` must be a NUL-terminated string and `pglob` a writable
/// `glob_t`, which with `GLOB_APPEND` must hold an earlier call's result.
/// `errfunc` must be null or accept a path and an error number.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn glob(
    pattern: *const c_char,
    flags: c_int,
    errfunc: ErrFunc,
    pglob: *mut Glob,
) -> c_int {
    // SAFETY: the caller passes a writable `glob_t`.
    let g = unsafe { &mut *pglob };
    let offs = if flags & GLOB_DOOFFS != 0 { g.gl_offs } else { 0 };
    if flags & GLOB_APPEND == 0 {
        g.gl_offs = offs;
        g.gl_pathc = 0;
        g.gl_pathv = null_mut();
    }
    // SAFETY: the caller passes a string.
    let pat = unsafe { CStr::from_ptr(pattern) }.to_bytes();
    let mut search = Search {
        flags,
        errfunc,
        found: Growable::new(),
    };
    let mut error = 0;
    if !pat.is_empty() {
        let mut buf: PathBuf = [0; PATH_MAX];
        let mut pos = 0;
        let mut rest = pat;
        if flags & (GLOB_TILDE | GLOB_TILDE_CHECK) != 0 && pat.first() == Some(&b'~') {
            match expand_tilde(pat, &mut buf) {
                Ok((at, after)) => {
                    pos = at;
                    rest = after;
                }
                Err(e) => error = e,
            }
        }
        if error == 0 {
            error = do_glob(&mut buf, pos, 0, rest, &mut search);
        }
    }
    if error == GLOB_NOSPACE {
        search.free_found();
        return error;
    }
    if search.found.len() == 0 {
        if flags & GLOB_NOCHECK != 0 {
            if !search.append(pat, false) {
                return GLOB_NOSPACE;
            }
        } else if error == 0 {
            return GLOB_NOMATCH;
        }
    }
    if flags & GLOB_NOSORT == 0 {
        sort_by(search.found.as_mut_slice(), |a, b| {
            // SAFETY: both are paths this call built.
            unsafe { strcmp(a, b) }.cmp(&0)
        });
    }
    // SAFETY: the caller's `glob_t` is valid, and with GLOB_APPEND holds an
    // earlier result.
    match unsafe { store(g, offs, flags & GLOB_APPEND != 0, &search.found) } {
        Some(()) => error,
        None => {
            search.free_found();
            GLOB_NOSPACE
        }
    }
}

/// Puts the paths `found` into `g`, after its `offs` reserved slots and, when
/// appending, its existing paths. Returns `None` if there is no memory.
///
/// # Safety
///
/// When appending, `g` must hold a result from `glob`.
unsafe fn store(g: &mut Glob, offs: usize, append: bool, found: &Growable<*mut c_char>) -> Option<()> {
    let count = found.len();
    let existing = if append { g.gl_pathc } else { 0 };
    let slots = offs.checked_add(existing)?.checked_add(count)?.checked_add(1)?;
    let bytes = slots.checked_mul(size_of::<*mut c_char>())?;
    let pathv = if append {
        // SAFETY: the vector came from an earlier call's `malloc`, or is null.
        unsafe { realloc(g.gl_pathv.cast(), bytes) }
    } else {
        malloc(bytes)
    }
    .cast::<*mut c_char>();
    if pathv.is_null() {
        return None;
    }
    if !append {
        let mut i = 0;
        while i < offs {
            // SAFETY: the vector holds `slots` entries.
            unsafe { pathv.wrapping_add(i).write(null_mut()) };
            i += 1;
        }
    }
    let base = offs + existing;
    for (i, &path) in found.as_slice().iter().enumerate() {
        // SAFETY: as above.
        unsafe { pathv.wrapping_add(base + i).write(path) };
    }
    // SAFETY: as above.
    unsafe { pathv.wrapping_add(base + count).write(null_mut()) };
    g.gl_pathv = pathv;
    g.gl_pathc = existing + count;
    Some(())
}

/// Frees the paths `glob` stored in `*pglob`.
///
/// # Safety
///
/// `pglob` must hold a result from `glob`, not used again until the next
/// call.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn globfree(pglob: *mut Glob) {
    // SAFETY: the caller passes a valid `glob_t`.
    let g = unsafe { &mut *pglob };
    if !g.gl_pathv.is_null() {
        let mut i = 0;
        while i < g.gl_pathc {
            // SAFETY: the vector holds `gl_offs + gl_pathc` entries.
            let path = unsafe { g.gl_pathv.wrapping_add(g.gl_offs + i).read() };
            // SAFETY: each path came from `malloc`.
            unsafe { free(path.cast()) };
            i += 1;
        }
    }
    // SAFETY: the vector came from `malloc`, or is null.
    unsafe { free(g.gl_pathv.cast()) };
    g.gl_pathc = 0;
    g.gl_pathv = null_mut();
}

/// `glob` under glibc's large-file name.
///
/// # Safety
///
/// As [`glob`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn glob64(
    pattern: *const c_char,
    flags: c_int,
    errfunc: ErrFunc,
    pglob: *mut Glob,
) -> c_int {
    // SAFETY: the same contract.
    unsafe { glob(pattern, flags, errfunc, pglob) }
}

/// `globfree` under glibc's large-file name.
///
/// # Safety
///
/// As [`globfree`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn globfree64(pglob: *mut Glob) {
    // SAFETY: the same contract.
    unsafe { globfree(pglob) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_literal_prefix_stops_at_the_component_with_a_wildcard() {
        let mut buf: PathBuf = [0; PATH_MAX];
        let mut pos = 0;
        let mut kind = DT_DIR;
        let mut pat: &[u8] = b"a\\b/c/d*e/f";
        assert!(literal_prefix(&mut buf, &mut pos, &mut kind, &mut pat, false));
        assert_eq!(span(&buf, 0, pos), b"ab/c/");
        assert_eq!(pat, b"d*e/f");
        assert_eq!(kind, 0);

        let mut pos = 0;
        let mut pat: &[u8] = b"x/y\\";
        assert!(!literal_prefix(&mut buf, &mut pos, &mut kind, &mut pat, false));

        let mut pos = 0;
        let mut pat: &[u8] = b"plain/name";
        assert!(literal_prefix(&mut buf, &mut pos, &mut kind, &mut pat, false));
        assert_eq!(span(&buf, 0, pos), b"plain/name");
        assert!(pat.is_empty());
    }
}
