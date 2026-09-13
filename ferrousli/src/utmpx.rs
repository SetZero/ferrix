//! `utmpx.h`, and glibc's `utmp.h` names for it: the records of who is logged
//! in, which this library does not keep.
//!
//! As in musl, there is no utmp or wtmp file. `getutxent`, `getutxid`,
//! `getutxline` and `pututxline` find and store nothing and return null;
//! `setutxent`, `endutxent` and `updwtmpx` do nothing; and `utmpxname` fails
//! with `ENOTSUP`. A program that lists who is logged in, as busybox's `who`
//! and `last` do, sees no one, and one that records a login, as `login` and
//! `getty` do, goes on without the record.

use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;

use crate::errno;

/// Rewinds the login records, of which there are none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setutxent() {}

/// Closes the login records, which were never opened.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endutxent() {}

/// The next login record: there is none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getutxent() -> *mut c_void {
    null_mut()
}

/// The record with the id in `*ut`: there is none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getutxid(_ut: *const c_void) -> *mut c_void {
    null_mut()
}

/// The record for the terminal line in `*ut`: there is none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getutxline(_ut: *const c_void) -> *mut c_void {
    null_mut()
}

/// Stores `*ut` in the login records: nothing is stored, and null says so.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pututxline(_ut: *const c_void) -> *mut c_void {
    null_mut()
}

/// Appends `*ut` to the history file `file`: nothing is written.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn updwtmpx(_file: *const c_char, _ut: *const c_void) {}

/// Chooses another records file: refused with `ENOTSUP`, which on Linux is
/// `EOPNOTSUPP`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn utmpxname(_file: *const c_char) -> c_int {
    errno::set(errno::EOPNOTSUPP);
    -1
}

/// `setutxent`, under `utmp.h`'s name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setutent() {}

/// `endutxent`, under `utmp.h`'s name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endutent() {}

/// `getutxent`, under `utmp.h`'s name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getutent() -> *mut c_void {
    null_mut()
}

/// `getutxid`, under `utmp.h`'s name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getutid(_ut: *const c_void) -> *mut c_void {
    null_mut()
}

/// `getutxline`, under `utmp.h`'s name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getutline(_ut: *const c_void) -> *mut c_void {
    null_mut()
}

/// `pututxline`, under `utmp.h`'s name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pututline(_ut: *const c_void) -> *mut c_void {
    null_mut()
}

/// `updwtmpx`, under `utmp.h`'s name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn updwtmp(_file: *const c_char, _ut: *const c_void) {}

/// `utmpxname`, under `utmp.h`'s name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn utmpname(file: *const c_char) -> c_int {
    utmpxname(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_no_records_and_no_other_file() {
        setutxent();
        assert!(getutxent().is_null());
        assert!(pututxline(core::ptr::null()).is_null());
        endutxent();
        assert_eq!(utmpname(c"/var/run/utmp".as_ptr()), -1);
        // SAFETY: the pointer is this thread's errno.
        let error = unsafe { errno::__errno_location().read() };
        assert_eq!(error, errno::EOPNOTSUPP);
    }
}
