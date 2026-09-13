//! `sys/utsname.h`'s `uname`, and `unistd.h`'s `gethostname` built on it.

use core::ffi::{c_char, c_int};
use core::mem::{MaybeUninit, offset_of, size_of};

use crate::errno;
use crate::syscall::{self, nr};

/// The length of each `struct utsname` field, `__NEW_UTS_LEN + 1` from
/// `linux/utsname.h`.
const FIELD: usize = 65;

/// C's `struct utsname`. The kernel's `struct new_utsname` is the same six
/// fields.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Utsname {
    /// The operating system, `Linux`.
    pub sysname: [c_char; FIELD],
    /// The host name.
    pub nodename: [c_char; FIELD],
    /// The kernel release.
    pub release: [c_char; FIELD],
    /// The kernel version.
    pub version: [c_char; FIELD],
    /// The hardware.
    pub machine: [c_char; FIELD],
    /// The NIS domain name.
    pub domainname: [c_char; FIELD],
}

const _: () = assert!(size_of::<Utsname>() == 6 * FIELD);
const _: () = assert!(offset_of!(Utsname, domainname) == 5 * FIELD);

/// Describes the system into `*buf`.
///
/// # Safety
///
/// `buf` must be valid for a write of a `struct utsname`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn uname(buf: *mut Utsname) -> c_int {
    // SAFETY: the kernel writes `buf`, as the caller vouches.
    let ret = unsafe { syscall::syscall2(nr::UNAME, buf.addr(), 0) };
    errno::from_syscall(ret) as c_int
}

/// How much of `name`, a NUL-terminated field, fits in `len` bytes with its
/// NUL: the bytes to copy, and whether the name fit whole.
fn fit(name: &[c_char], len: usize) -> (usize, bool) {
    let length = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    if length < len {
        (length, true)
    } else {
        (len.saturating_sub(1), false)
    }
}

/// Copies the host name into the `len` bytes at `name`.
///
/// A name that does not fit with its NUL is truncated, still terminated if
/// `len` is not zero, and the call fails with `ENAMETOOLONG`, so a caller can
/// tell a truncated name from a whole one.
///
/// # Safety
///
/// `name` must be valid for writes of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostname(name: *mut c_char, len: usize) -> c_int {
    let mut uts = MaybeUninit::<Utsname>::uninit();
    // SAFETY: the kernel writes the whole structure into the live local.
    if unsafe { uname(uts.as_mut_ptr()) } != 0 {
        return -1;
    }
    // SAFETY: `uname` succeeded, so the kernel initialised every field.
    let uts = unsafe { uts.assume_init() };
    let (copy, whole) = fit(&uts.nodename, len);
    let mut i = 0;
    while i < copy {
        // SAFETY: `copy < len`, so the write is inside the caller's buffer,
        // and `copy` is at most the field's length.
        unsafe {
            name.wrapping_add(i)
                .write(*uts.nodename.get(i).unwrap_or(&0))
        };
        i += 1;
    }
    if len > 0 {
        // SAFETY: `copy < len`, so the terminator is inside the buffer.
        unsafe { name.wrapping_add(copy).write(0) };
    }
    if whole {
        0
    } else {
        errno::set(errno::ENAMETOOLONG);
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_fits_only_with_room_for_its_nul() {
        let name = [b'a' as c_char, b'b' as c_char, 0, 0];
        assert_eq!(fit(&name, 3), (2, true));
        assert_eq!(fit(&name, 2), (1, false));
        assert_eq!(fit(&name, 0), (0, false));
    }

    #[test]
    fn uname_says_linux() {
        let mut uts = MaybeUninit::<Utsname>::uninit();
        // SAFETY: the structure is a live local.
        assert_eq!(unsafe { uname(uts.as_mut_ptr()) }, 0);
        // SAFETY: `uname` succeeded.
        let uts = unsafe { uts.assume_init() };
        assert_eq!(fit(&uts.sysname, FIELD), (5, true));
        assert_eq!(
            uts.sysname.get(..5),
            Some(&b"Linux".map(|b| b as c_char)[..])
        );
    }
}
