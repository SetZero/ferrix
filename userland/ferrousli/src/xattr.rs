//! `sys/xattr.h`: extended attributes, by path, by path without following a
//! final symbolic link, and by descriptor.
//!
//! Each is its system call with `errno` set from the result; there is nothing
//! for a C library to add.

use core::ffi::{c_char, c_int, c_void};

use crate::errno;
use crate::syscall::{self, nr};

/// A byte count, or -1 with `errno` set.
fn count(ret: isize) -> isize {
    match errno::decode(ret) {
        Ok(value) => value as isize,
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// Zero, or -1 with `errno` set.
fn status(ret: isize) -> c_int {
    count(ret).min(0) as c_int
}

/// Declares the three forms of a call that reads into a buffer.
macro_rules! getter {
    ($(#[$doc:meta])* $name:ident, $nr:ident, $target:ty) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// The name must be a NUL-terminated string and `value` valid for
        /// writes of `size` bytes, or `size` zero.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name(
            target: $target,
            name: *const c_char,
            value: *mut c_void,
            size: usize,
        ) -> isize {
            // SAFETY: the caller vouches for every pointer the kernel reads
            // or writes.
            count(unsafe {
                syscall::syscall4(nr::$nr, target as usize, name.addr(), value.addr(), size)
            })
        }
    };
}

getter!(
    /// The value of attribute `name` of the file at `path`.
    getxattr, GETXATTR, *const c_char
);
getter!(
    /// [`getxattr`], of a symbolic link itself.
    lgetxattr, LGETXATTR, *const c_char
);
getter!(
    /// [`getxattr`], of an open file.
    fgetxattr, FGETXATTR, c_int
);

/// Declares the three forms of a call that sets a value.
macro_rules! setter {
    ($(#[$doc:meta])* $name:ident, $nr:ident, $target:ty) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// The name must be a NUL-terminated string and `value` valid for
        /// reads of `size` bytes.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name(
            target: $target,
            name: *const c_char,
            value: *const c_void,
            size: usize,
            flags: c_int,
        ) -> c_int {
            // SAFETY: the caller vouches for every pointer the kernel reads.
            status(unsafe {
                syscall::syscall6(
                    nr::$nr,
                    target as usize,
                    name.addr(),
                    value.addr(),
                    size,
                    flags as usize,
                    0,
                )
            })
        }
    };
}

setter!(
    /// Sets attribute `name` of the file at `path`.
    setxattr, SETXATTR, *const c_char
);
setter!(
    /// [`setxattr`], on a symbolic link itself.
    lsetxattr, LSETXATTR, *const c_char
);
setter!(
    /// [`setxattr`], on an open file.
    fsetxattr, FSETXATTR, c_int
);

/// Declares the three forms of a call that lists names.
macro_rules! lister {
    ($(#[$doc:meta])* $name:ident, $nr:ident, $target:ty) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// `list` must be valid for writes of `size` bytes, or `size` zero.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name(target: $target, list: *mut c_char, size: usize) -> isize {
            // SAFETY: the caller vouches for the buffer.
            count(unsafe { syscall::syscall3(nr::$nr, target as usize, list.addr(), size) })
        }
    };
}

lister!(
    /// The names of the file at `path`'s attributes, each ending in a NUL.
    listxattr, LISTXATTR, *const c_char
);
lister!(
    /// [`listxattr`], of a symbolic link itself.
    llistxattr, LLISTXATTR, *const c_char
);
lister!(
    /// [`listxattr`], of an open file.
    flistxattr, FLISTXATTR, c_int
);

/// Declares the three forms of a call that removes an attribute.
macro_rules! remover {
    ($(#[$doc:meta])* $name:ident, $nr:ident, $target:ty) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// The name must be a NUL-terminated string.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name(target: $target, name: *const c_char) -> c_int {
            // SAFETY: the caller vouches for the name.
            status(unsafe { syscall::syscall2(nr::$nr, target as usize, name.addr()) })
        }
    };
}

remover!(
    /// Removes attribute `name` of the file at `path`.
    removexattr, REMOVEXATTR, *const c_char
);
remover!(
    /// [`removexattr`], of a symbolic link itself.
    lremovexattr, LREMOVEXATTR, *const c_char
);
remover!(
    /// [`removexattr`], of an open file.
    fremovexattr, FREMOVEXATTR, c_int
);
