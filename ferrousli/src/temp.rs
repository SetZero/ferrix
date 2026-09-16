//! `stdlib.h`'s temporary files and directories: `mkstemp`, `mkostemp`,
//! `mkstemps`, `mkostemps`, `mkdtemp` and `mktemp`.
//!
//! Each follows musl. The six `X`s a template ends in, before a suffix of the
//! given length, are replaced with letters from `A` to `P` and `a` to `p`
//! drawn from the clock and the thread id. A name already taken is tried
//! again with new letters, up to 100 times, after which the `X`s are put back.
//! A file is created exclusively and readable and writable by its owner only,
//! a directory is created for its owner only.
//!
//! `mktemp` only picks a name that did not exist when it looked, which another
//! program can take before the caller uses it. It is here because programs
//! still call it, and so is `stdio.h`'s `tmpnam`, which has the same flaw and
//! is obsolescent in POSIX.1-2024. glibc's `*64` names are the same functions.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};
use core::ptr::null_mut;

use crate::errno;
use crate::fcntl::{O_CREAT, open};
use crate::stat::{Stat, mkdir, stat};
use crate::string::strlen;
use crate::syscall::{self, nr};
use crate::time::{CLOCK_REALTIME, Timespec, clock_gettime};

/// `O_RDWR`, from `asm-generic/fcntl.h`.
const O_RDWR: c_int = 0o2;
/// `O_EXCL`, from `asm-generic/fcntl.h`.
const O_EXCL: c_int = 0o200;
/// `O_ACCMODE`, from `asm-generic/fcntl.h`.
const O_ACCMODE: c_int = 0o3;
/// How many names are tried.
const TRIES: usize = 100;
/// What a template's name part must be.
const XS: &[u8; 6] = b"XXXXXX";

/// The calling thread's `errno`.
fn last_errno() -> c_int {
    // SAFETY: the pointer is this thread's errno.
    unsafe { errno::__errno_location().read() }
}

/// Where the six `X`s start in `template`, which ends in a suffix of `suffix`
/// bytes, or `None` if they are not there.
///
/// # Safety
///
/// `template` must be a NUL-terminated string.
unsafe fn xs_start(template: *const c_char, suffix: c_int) -> Option<usize> {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(template) };
    let suffix = usize::try_from(suffix).ok()?;
    let start = len.checked_sub(XS.len())?.checked_sub(suffix)?;
    for (offset, &x) in XS.iter().enumerate() {
        // SAFETY: `start + offset` is below `len`.
        let byte = unsafe { template.wrapping_add(start + offset).read() };
        if byte as u8 != x {
            return None;
        }
    }
    Some(start)
}

/// The letters for `bits`, as musl's `__randname` picks them.
fn letters(mut bits: u64) -> [u8; 6] {
    let mut name = [0u8; 6];
    for letter in &mut name {
        *letter = b'A' + (bits & 15) as u8 + ((bits & 16) * 2) as u8;
        bits >>= 5;
    }
    name
}

/// Writes six letters from the clock and the thread id at `at`.
///
/// # Safety
///
/// `at` must be valid for writes of six bytes.
unsafe fn random_name(at: *mut c_char) {
    let mut now = Timespec::default();
    // SAFETY: `now` is a live local.
    let _ = unsafe { clock_gettime(CLOCK_REALTIME, &raw mut now) };
    // SAFETY: `gettid` reads no memory.
    let tid = unsafe { syscall::syscall0(nr::GETTID) } as u64;
    let bits = (now.tv_sec as u64)
        .wrapping_add(now.tv_nsec as u64)
        .wrapping_add(tid.wrapping_mul(65537));
    write(at, letters(bits));
}

/// Writes `name` at `at`.
fn write(at: *mut c_char, name: [u8; 6]) {
    for (offset, byte) in name.into_iter().enumerate() {
        // SAFETY: every caller passes six writable bytes at `at`.
        unsafe { at.wrapping_add(offset).write(byte as c_char) };
    }
}

/// Creates and opens a file named after `template`, whose last `suffix` bytes
/// follow the six `X`s, with `flags` beyond `O_RDWR`, `O_CREAT` and `O_EXCL`,
/// and returns its descriptor. A template without the `X`s fails with
/// `EINVAL`.
///
/// # Safety
///
/// `template` must be a writable NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkostemps(template: *mut c_char, suffix: c_int, flags: c_int) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let Some(start) = (unsafe { xs_start(template, suffix) }) else {
        errno::set(errno::EINVAL);
        return -1;
    };
    let xs = template.wrapping_add(start);
    let flags = (flags & !O_ACCMODE) | O_RDWR | O_CREAT | O_EXCL;
    for _ in 0..TRIES {
        // SAFETY: the six `X`s are writable bytes of the template.
        unsafe { random_name(xs) };
        // SAFETY: the template is still NUL-terminated.
        let fd = unsafe { open(template, flags, 0o600) };
        if fd >= 0 {
            return fd;
        }
        if last_errno() != errno::EEXIST {
            break;
        }
    }
    write(xs, *XS);
    -1
}

/// `mkostemps` without a suffix.
///
/// # Safety
///
/// As `mkostemps`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkostemp(template: *mut c_char, flags: c_int) -> c_int {
    // SAFETY: the caller's promises are `mkostemps`'s.
    unsafe { mkostemps(template, 0, flags) }
}

/// `mkostemps` without further flags.
///
/// # Safety
///
/// As `mkostemps`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkstemps(template: *mut c_char, suffix: c_int) -> c_int {
    // SAFETY: the caller's promises are `mkostemps`'s.
    unsafe { mkostemps(template, suffix, 0) }
}

/// `mkostemps` without a suffix or further flags.
///
/// # Safety
///
/// As `mkostemps`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkstemp(template: *mut c_char) -> c_int {
    // SAFETY: the caller's promises are `mkostemps`'s.
    unsafe { mkostemps(template, 0, 0) }
}

/// `mkstemp`, under glibc's large-file name.
///
/// # Safety
///
/// As `mkostemps`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkstemp64(template: *mut c_char) -> c_int {
    // SAFETY: the caller's promises are `mkostemps`'s.
    unsafe { mkostemps(template, 0, 0) }
}

/// `mkostemp`, under glibc's large-file name.
///
/// # Safety
///
/// As `mkostemps`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkostemp64(template: *mut c_char, flags: c_int) -> c_int {
    // SAFETY: the caller's promises are `mkostemps`'s.
    unsafe { mkostemps(template, 0, flags) }
}

/// Creates a directory named after `template`, which ends in six `X`s, and
/// returns `template`, or null with `errno` set.
///
/// # Safety
///
/// `template` must be a writable NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mkdtemp(template: *mut c_char) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated string.
    let Some(start) = (unsafe { xs_start(template, 0) }) else {
        errno::set(errno::EINVAL);
        return null_mut();
    };
    let xs = template.wrapping_add(start);
    for _ in 0..TRIES {
        // SAFETY: the six `X`s are writable bytes of the template.
        unsafe { random_name(xs) };
        // SAFETY: the template is still NUL-terminated.
        if unsafe { mkdir(template, 0o700) } == 0 {
            return template;
        }
        if last_errno() != errno::EEXIST {
            break;
        }
    }
    write(xs, *XS);
    null_mut()
}

/// Replaces the six `X`s that end `template` with a name that does not exist
/// yet, and returns `template`. On failure the template is made empty, with
/// `errno` set.
///
/// # Safety
///
/// `template` must be a writable NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mktemp(template: *mut c_char) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated string.
    let Some(start) = (unsafe { xs_start(template, 0) }) else {
        errno::set(errno::EINVAL);
        // SAFETY: a NUL-terminated string has at least its NUL.
        unsafe { template.write(0) };
        return template;
    };
    let xs = template.wrapping_add(start);
    let mut info = Stat::default();
    for _ in 0..TRIES {
        // SAFETY: the six `X`s are writable bytes of the template.
        unsafe { random_name(xs) };
        // SAFETY: the template is NUL-terminated, and `info` a live local.
        if unsafe { stat(template, &raw mut info) } != 0 {
            if last_errno() != errno::ENOENT {
                // SAFETY: as above.
                unsafe { template.write(0) };
            }
            return template;
        }
    }
    // SAFETY: as above.
    unsafe { template.write(0) };
    errno::set(errno::EEXIST);
    template
}

/// `L_tmpnam`, from `include/stdio.h`: the size of a buffer `tmpnam` fills.
const L_TMPNAM: usize = 20;

/// The buffer `tmpnam` returns when given none.
#[derive(Debug)]
struct NameBuffer(UnsafeCell<[u8; L_TMPNAM]>);

// SAFETY: C documents `tmpnam`'s own result as static storage the next call
// overwrites, and a program calling it from two threads at once as passing its
// own buffers, as with musl's.
unsafe impl Sync for NameBuffer {}

/// What `tmpnam` returns for a null buffer.
static NAME: NameBuffer = NameBuffer(UnsafeCell::new([0; L_TMPNAM]));

/// A name under `/tmp` that no file had when it looked, copied into `buffer`,
/// or into static storage the next call overwrites when `buffer` is null.
/// Returns where it was written, or null if 100 names were all taken.
///
/// musl's `stdio/tmpnam.c` (MIT): `/tmp/tmpnam_` and six letters, each tried
/// with `readlink`, which fails with `ENOENT` only when nothing has the name.
///
/// # Safety
///
/// `buffer` must be null or valid for writes of `L_tmpnam` (20) bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tmpnam(buffer: *mut c_char) -> *mut c_char {
    let mut name = *b"/tmp/tmpnam_XXXXXX\0";
    let mut probe = [0u8; 1];
    for _ in 0..TRIES {
        // SAFETY: bytes 12 to 17 of `name` are the six writable `X`s.
        unsafe { random_name(name.as_mut_ptr().wrapping_add(12).cast()) };
        // SAFETY: `name` is NUL-terminated, and `probe` has room for the one
        // byte the call may write.
        let ret = unsafe {
            syscall::syscall3(
                nr::READLINK,
                name.as_ptr().addr(),
                probe.as_mut_ptr().addr(),
                1,
            )
        };
        if errno::decode(ret) == Err(errno::ENOENT) {
            let out = if buffer.is_null() {
                NAME.0.get().cast::<c_char>()
            } else {
                buffer
            };
            for (offset, byte) in name.into_iter().enumerate() {
                // SAFETY: `name` is 19 bytes with its NUL, and `out` has room
                // for 20.
                unsafe { out.wrapping_add(offset).write(byte as c_char) };
            }
            return out;
        }
    }
    null_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_are_from_a_to_p_in_either_case() {
        for bits in [0, 1, 15, 16, 31, u64::MAX, 0x0123_4567_89ab_cdef] {
            for letter in letters(bits) {
                assert!(matches!(letter, b'A'..=b'P' | b'a'..=b'p'), "{letter}");
            }
        }
        assert_eq!(&letters(0), b"AAAAAA");
        assert_eq!(&letters(31), b"pAAAAA");
    }

    #[test]
    fn the_xs_are_found_before_the_suffix_or_refused() {
        let cases: [(&core::ffi::CStr, c_int, Option<usize>); 7] = [
            (c"fileXXXXXX", 0, Some(4)),
            (c"partXXXXXX.txt", 4, Some(4)),
            (c"XXXXXX", 0, Some(0)),
            (c"fileXXXXX", 0, None),
            (c"XXXXXXab", 3, None),
            (c"XXXXXX", -1, None),
            (c"fileXXXXXx", 0, None),
        ];
        for (template, suffix, start) in cases {
            // SAFETY: every template is NUL-terminated.
            let found = unsafe { xs_start(template.as_ptr(), suffix) };
            assert_eq!(found, start, "{template:?}");
        }
    }
}
