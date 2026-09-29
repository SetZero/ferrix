//! `ftw.h`: `nftw` and `ftw`, and glibc's `nftw64` and `ftw64`, which GLib
//! calls.
//!
//! Ported from musl 1.2.5's `src/misc/nftw.c` and `ftw.c` (MIT; see
//! [`crate::math`] for the notice), with three changes that make it glibc's:
//!
//! * The type flags a callback is given are glibc's, `FTW_F` 0 to
//!   `FTW_SLN` 6, where musl's run from 1. A program built against glibc
//!   compares with glibc's numbers; `include/ftw.h` says the same now, an
//!   edit that says it is ferrousli's.
//! * `FTW_ACTIONRETVAL`, glibc's, lets a callback answer `FTW_SKIP_SUBTREE`
//!   or `FTW_SKIP_SIBLINGS` as well as `FTW_CONTINUE` and `FTW_STOP`.
//! * The descriptor limit does not limit the depth, as musl's does: one
//!   descriptor is open for each directory being read, as in glibc before
//!   it starts closing and reopening them.
//!
//! `FTW_CHDIR` is accepted and, as in musl, the working directory is not
//! changed: every path given to the callback is whole.

use core::ffi::{c_char, c_int};

use crate::dirent::{Dir, closedir, fdopendir, readdir};
use crate::errno;
use crate::stat::{Stat, lstat, stat};
use crate::syscall::{self, nr};

/// A regular file, or anything that is not a directory or a link.
const FTW_F: c_int = 0;
/// A directory, before its entries.
const FTW_D: c_int = 1;
/// A directory that cannot be read.
const FTW_DNR: c_int = 2;
/// A file that cannot be `stat`ed.
const FTW_NS: c_int = 3;
/// A symbolic link, with `FTW_PHYS`.
const FTW_SL: c_int = 4;
/// A directory, after its entries, with `FTW_DEPTH`.
const FTW_DP: c_int = 5;
/// A symbolic link to nothing.
const FTW_SLN: c_int = 6;

/// Do not follow symbolic links.
const FTW_PHYS: c_int = 1;
/// Stay on the starting file system.
const FTW_MOUNT: c_int = 2;
/// Report a directory after its entries.
const FTW_DEPTH: c_int = 8;
/// The callback's answer says how to go on.
const FTW_ACTIONRETVAL: c_int = 16;

/// With `FTW_ACTIONRETVAL`: go on.
const FTW_CONTINUE: c_int = 0;
/// With `FTW_ACTIONRETVAL`: do not read this directory's entries.
const FTW_SKIP_SUBTREE: c_int = 2;
/// With `FTW_ACTIONRETVAL`: skip the rest of this directory's entries.
const FTW_SKIP_SIBLINGS: c_int = 3;

/// `PATH_MAX`.
const PATH_MAX: usize = 4096;

/// C's `struct FTW`: where the last component of the path starts, and how
/// deep it is below the starting path.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Ftw {
    /// The offset of the file's name in the path.
    pub base: c_int,
    /// Its depth, the starting path being 0.
    pub level: c_int,
}

/// `nftw`'s callback.
pub type NftwFn = unsafe extern "C" fn(*const c_char, *const Stat, c_int, *mut Ftw) -> c_int;
/// `ftw`'s callback.
pub type FtwFn = unsafe extern "C" fn(*const c_char, *const Stat, c_int) -> c_int;

/// A directory being read, and the ones it is in.
struct History<'a> {
    /// The directory it is in.
    chain: Option<&'a History<'a>>,
    /// Its device.
    dev: u64,
    /// Its inode.
    ino: u64,
    /// Its depth.
    level: c_int,
    /// Where its entries' names start in the path.
    base: usize,
}

/// How a step of the walk ends.
enum Step {
    /// Go on.
    Continue,
    /// Skip the rest of the directory the step was in.
    SkipSiblings,
    /// Stop, and return this from `nftw`.
    Stop(c_int),
}

/// What the callback's answer `r` means under `flags`.
fn step_for(r: c_int, flags: c_int, kind: c_int) -> (Step, bool) {
    if flags & FTW_ACTIONRETVAL == 0 {
        return (
            if r == 0 {
                Step::Continue
            } else {
                Step::Stop(r)
            },
            false,
        );
    }
    match r {
        FTW_CONTINUE => (Step::Continue, false),
        FTW_SKIP_SUBTREE => (Step::Continue, kind == FTW_D),
        FTW_SKIP_SIBLINGS => (Step::SkipSiblings, true),
        _ => (Step::Stop(r), false),
    }
}

/// Closes `fd`, ignoring the answer.
fn close(fd: c_int) {
    // SAFETY: `close` reads no memory.
    let _ = unsafe { syscall::syscall2(nr::CLOSE, fd as usize, 0) };
}

/// Walks the file at `path[..len]`, whose buffer holds `PATH_MAX + 1`
/// bytes, then, if it is a directory, the tree below it. musl's `do_nftw`.
///
/// # Safety
///
/// `path` must hold a NUL at `len`, and `callback` must be sound to call.
unsafe fn walk(
    path: &mut [c_char; PATH_MAX + 1],
    len: usize,
    callback: NftwFn,
    flags: c_int,
    parent: Option<&History<'_>>,
) -> Step {
    let bytes = path.as_ptr();
    let last_is_slash = len > 0 && path.get(len - 1).is_some_and(|&b| b == b'/' as c_char);
    let end = if last_is_slash { len - 1 } else { len };
    let mut st = Stat::default();

    let follow = flags & FTW_PHYS == 0;
    let ret = if follow {
        // SAFETY: `path` holds a NUL-terminated string, and `st` is a local.
        unsafe { stat(bytes, &raw mut st) }
    } else {
        // SAFETY: as above.
        unsafe { lstat(bytes, &raw mut st) }
    };
    let failed = ret < 0;
    let mode = st.st_mode & 0o170_000;
    let mut kind = if failed {
        let error = errno::get();
        // SAFETY: as above.
        if follow && error == errno::ENOENT && unsafe { lstat(bytes, &raw mut st) } == 0 {
            FTW_SLN
        } else if error != errno::EACCES {
            return Step::Stop(-1);
        } else {
            FTW_NS
        }
    } else if mode == 0o040_000 {
        if flags & FTW_DEPTH != 0 {
            FTW_DP
        } else {
            FTW_D
        }
    } else if mode == 0o120_000 {
        if follow { FTW_SLN } else { FTW_SL }
    } else {
        FTW_F
    };

    if flags & FTW_MOUNT != 0
        && let Some(parent) = parent
        && kind != FTW_NS
        && st.st_dev != parent.dev
    {
        return Step::Continue;
    }

    let level = parent.map_or(0, |parent| parent.level + 1);
    let base = match parent {
        Some(parent) => parent.base,
        None => {
            let mut k = end;
            while k > 0 && path.get(k).is_some_and(|&b| b == b'/' as c_char) {
                k -= 1;
            }
            while k > 0 && path.get(k - 1).is_some_and(|&b| b != b'/' as c_char) {
                k -= 1;
            }
            k
        }
    };
    let here = History {
        chain: parent,
        dev: st.st_dev,
        ino: st.st_ino,
        level,
        base: end + 1,
    };
    let mut position = Ftw {
        base: base as c_int,
        level,
    };

    let mut dfd = -1;
    let mut open_error = 0;
    if kind == FTW_D || kind == FTW_DP {
        // SAFETY: as above.
        dfd = unsafe { crate::fcntl::open(bytes, crate::fcntl::O_DIRECTORY | 0o2_000_000, 0) };
        if dfd < 0 {
            open_error = errno::get();
            if open_error == errno::EACCES {
                kind = FTW_DNR;
            }
        }
    }

    let mut skip_subtree = false;
    if flags & FTW_DEPTH == 0 {
        // SAFETY: the caller vouches for the callback.
        let r = unsafe { callback(bytes, &raw const st, kind, &raw mut position) };
        let (step, skip) = step_for(r, flags, kind);
        if !matches!(step, Step::Continue) {
            if dfd >= 0 {
                close(dfd);
            }
            return step;
        }
        skip_subtree = skip;
    }

    let mut ancestor = parent;
    while let Some(h) = ancestor {
        if h.dev == st.st_dev && h.ino == st.st_ino {
            if dfd >= 0 {
                close(dfd);
            }
            return Step::Continue;
        }
        ancestor = h.chain;
    }

    if (kind == FTW_D || kind == FTW_DP) && !skip_subtree {
        if dfd < 0 {
            errno::set(open_error);
            return Step::Stop(-1);
        }
        // SAFETY: the stream owns `dfd` from here.
        let dir = fdopendir(dfd);
        if dir.is_null() {
            close(dfd);
            return Step::Stop(-1);
        }
        // SAFETY: the stream is this call's own, and `path` holds `len`
        // bytes and a NUL.
        let step = unsafe { entries(dir, path, len, end, callback, flags, &here) };
        // SAFETY: the stream is not used again.
        let _ = unsafe { closedir(dir) };
        if let Step::Stop(r) = step {
            return Step::Stop(r);
        }
    } else if dfd >= 0 {
        close(dfd);
    }

    if let Some(slot) = path.get_mut(len) {
        *slot = 0;
    }
    if flags & FTW_DEPTH != 0 {
        // SAFETY: as above.
        let r = unsafe { callback(bytes, &raw const st, kind, &raw mut position) };
        return step_for(r, flags, kind).0;
    }
    Step::Continue
}

/// Walks each entry of `dir` but `.` and `..`, the directory at
/// `path[..len]`, whose entries' names go after `path[end]`.
///
/// # Safety
///
/// As [`walk`], and `dir` must be a stream nothing else uses.
unsafe fn entries(
    dir: *mut Dir,
    path: &mut [c_char; PATH_MAX + 1],
    len: usize,
    end: usize,
    callback: NftwFn,
    flags: c_int,
    here: &History<'_>,
) -> Step {
    loop {
        // SAFETY: the caller's stream.
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            return Step::Continue;
        }
        // SAFETY: `readdir` returned a whole entry.
        let name = unsafe { &(*entry).d_name };
        let name_len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        let dots = match name_len {
            1 => name.first() == Some(&(b'.' as c_char)),
            2 => name.iter().take(2).all(|&b| b == b'.' as c_char),
            _ => false,
        };
        if dots {
            continue;
        }
        if name_len >= PATH_MAX - len {
            errno::set(errno::ENAMETOOLONG);
            return Step::Stop(-1);
        }
        if let Some(slot) = path.get_mut(end) {
            *slot = b'/' as c_char;
        }
        let tail = path.iter_mut().skip(end + 1);
        for (slot, &byte) in tail.zip(name.iter().take(name_len).chain(&[0])) {
            *slot = byte;
        }
        // SAFETY: `path` holds the entry's path and a NUL now.
        match unsafe { walk(path, end + 1 + name_len, callback, flags, Some(here)) } {
            Step::Continue => {}
            Step::SkipSiblings => return Step::Continue,
            Step::Stop(r) => return Step::Stop(r),
        }
    }
}

/// Walks the tree at `path`, calling `callback` for each file with its
/// path, its status, its type and where it is, until one call answers
/// nonzero, which `nftw` then returns. -1 with `errno` set on an error.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `callback` sound to call
/// with each file.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn nftw(
    path: *const c_char,
    callback: Option<NftwFn>,
    fd_limit: c_int,
    flags: c_int,
) -> c_int {
    let Some(callback) = callback else {
        errno::set(errno::EINVAL);
        return -1;
    };
    if fd_limit <= 0 {
        return 0;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { crate::string::strlen(path) };
    if len > PATH_MAX {
        errno::set(errno::ENAMETOOLONG);
        return -1;
    }
    let mut buffer = [0 as c_char; PATH_MAX + 1];
    // SAFETY: `len` bytes and the NUL fit in the buffer.
    let _ = unsafe { crate::string::memcpy(buffer.as_mut_ptr().cast(), path.cast(), len + 1) };
    // SAFETY: the buffer holds the path and its NUL.
    match unsafe { walk(&mut buffer, len, callback, flags, None) } {
        Step::Continue | Step::SkipSiblings => 0,
        Step::Stop(r) => r,
    }
}

/// glibc's large-file name for [`nftw`]: every file is large here.
///
/// # Safety
///
/// As [`nftw`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn nftw64(
    path: *const c_char,
    callback: Option<NftwFn>,
    fd_limit: c_int,
    flags: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `nftw`'s.
    unsafe { nftw(path, callback, fd_limit, flags) }
}

/// [`nftw`] without the position, not following symbolic links: musl's
/// `ftw`. The callback takes three arguments, and a fourth passed to it is
/// ignored, as the calling convention allows.
///
/// # Safety
///
/// As [`nftw`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ftw(
    path: *const c_char,
    callback: Option<FtwFn>,
    fd_limit: c_int,
) -> c_int {
    let callback = callback.map(|f| {
        // SAFETY: a three-argument C function called with four ignores the
        // fourth on every architecture here, as musl's `ftw` relies on.
        unsafe { core::mem::transmute::<FtwFn, NftwFn>(f) }
    });
    // SAFETY: as above.
    unsafe { nftw(path, callback, fd_limit, FTW_PHYS) }
}

/// glibc's large-file name for [`ftw`].
///
/// # Safety
///
/// As [`ftw`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ftw64(
    path: *const c_char,
    callback: Option<FtwFn>,
    fd_limit: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `ftw`'s.
    unsafe { ftw(path, callback, fd_limit) }
}
