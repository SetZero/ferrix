//! The Linux calls the runtime has no function for, made by number.
//!
//! An app changes nothing outside its folder (`docs/APPS.md` §4), so the
//! three calls this program needs beyond `read`, `write` and `close` are
//! spelt here, through `ferrix_rt::linux::call`, with the running
//! architecture's numbers from `ferrix_rt::linux::numbers`. The layouts they
//! use are the same on all three architectures: a path, a buffer of
//! `linux_dirent64` records, and `struct new_utsname`, six 65-byte fields.

use ferrix_rt::linux::{self, numbers};

/// `AT_FDCWD`: start from the current directory, which an absolute path then
/// ignores.
const AT_FDCWD: usize = (-100_isize) as usize;
/// `O_RDONLY | O_CLOEXEC`: `O_CLOEXEC` is the same on every architecture,
/// where `O_DIRECTORY` is not, and a directory opens without it.
const READ_ONLY: usize = 0o2_000_000;

/// `openat(AT_FDCWD, path, O_RDONLY | O_CLOEXEC)`: a file or a directory, for
/// reading, or `None`.
pub(crate) fn open(path: &[u8]) -> Option<usize> {
    if !path.contains(&0) {
        return None;
    }
    let at = path.as_ptr().addr();
    // SAFETY: `path` is borrowed for the call and holds a NUL, checked above,
    // so the kernel's read of the string ends inside it; the kernel only
    // reads it.
    unsafe { linux::call(numbers::OPENAT, [AT_FDCWD, at, READ_ONLY, 0, 0, 0]) }.ok()
}

/// `getdents64(fd, bytes)`: how many bytes of the directory's next entries
/// it wrote, zero at the end, or `None`.
pub(crate) fn getdents64(fd: usize, bytes: &mut [u8]) -> Option<usize> {
    let at = bytes.as_mut_ptr().addr();
    let len = bytes.len();
    // SAFETY: `bytes` is borrowed exclusively for the call and is `len`
    // bytes; the kernel writes at most that many.
    unsafe { linux::call(numbers::GETDENTS64, [fd, at, len, 0, 0, 0]) }.ok()
}

/// `uname(names)`: whether the kernel filled `names`, a `struct
/// new_utsname`.
pub(crate) fn uname(names: &mut [u8; ferrofetch::parse::UTSNAME]) -> bool {
    let at = names.as_mut_ptr().addr();
    // SAFETY: `names` is borrowed exclusively for the call and is exactly a
    // `struct new_utsname`'s size, which is all the kernel writes.
    unsafe { linux::call(numbers::UNAME, [at, 0, 0, 0, 0, 0]) }.is_ok()
}
