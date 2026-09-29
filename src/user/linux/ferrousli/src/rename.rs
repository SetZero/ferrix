//! `rename`, `renameat` and `renameat2`.
//!
//! `stdio.h` declares them, but they are file system calls with nothing to do
//! with streams, so they live here rather than in [`crate::stdio`]. All are
//! the kernel's `renameat2`, the first two with no flags, which every
//! architecture has; AArch64 has neither `rename` nor `renameat`.

use core::ffi::{c_char, c_int, c_uint};

use crate::errno;
use crate::fcntl::AT_FDCWD;
use crate::syscall::{self, nr};

/// Renames `old`, relative to `olddirfd`, to `new`, relative to `newdirfd`,
/// replacing anything already at `new`.
///
/// # Safety
///
/// Both paths must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn renameat(
    olddirfd: c_int,
    old: *const c_char,
    newdirfd: c_int,
    new: *const c_char,
) -> c_int {
    // SAFETY: the caller's contract is `renameat2`'s.
    unsafe { renameat2(olddirfd, old, newdirfd, new, 0) }
}

/// [`renameat`], with `flags`: `RENAME_NOREPLACE`, `RENAME_EXCHANGE` or
/// `RENAME_WHITEOUT`, which the kernel checks. GNU `tar` asks for it.
///
/// # Safety
///
/// Both paths must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn renameat2(
    olddirfd: c_int,
    old: *const c_char,
    newdirfd: c_int,
    new: *const c_char,
    flags: c_uint,
) -> c_int {
    // SAFETY: the kernel only reads the two paths.
    let ret = unsafe {
        syscall::syscall6(
            nr::RENAMEAT2,
            olddirfd as usize,
            old.addr(),
            newdirfd as usize,
            new.addr(),
            flags as usize,
            0,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Renames `old` to `new`, replacing anything already at `new`.
///
/// # Safety
///
/// Both paths must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn rename(old: *const c_char, new: *const c_char) -> c_int {
    // SAFETY: the caller's contract is `renameat`'s.
    unsafe { renameat(AT_FDCWD, old, AT_FDCWD, new) }
}
