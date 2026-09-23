//! The auxiliary vector: what the kernel tells a program about itself.
//!
//! The kernel places it on the initial stack after the environment's null, as
//! key and value pairs ending in `AT_NULL`. It stays there for the life of the
//! process, so the library keeps a pointer rather than a copy.

use core::ffi::c_ulong;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::errno;

/// The end of the vector.
pub const AT_NULL: usize = 0;
/// The address of the program's headers.
pub const AT_PHDR: usize = 3;
/// The size of one program header.
pub const AT_PHENT: usize = 4;
/// The number of program headers.
pub const AT_PHNUM: usize = 5;
/// The page size.
pub const AT_PAGESZ: usize = 6;
/// The address of sixteen random bytes.
pub const AT_RANDOM: usize = 25;

/// The vector, or null before [`init`].
static AUXV: AtomicPtr<usize> = AtomicPtr::new(null_mut());

/// Records where the vector is.
///
/// # Safety
///
/// `auxv` must point at key and value pairs ending in `AT_NULL`, which stay in
/// place for as long as anything calls [`get`].
pub unsafe fn init(auxv: *const usize) {
    AUXV.store(auxv.cast_mut(), Ordering::Relaxed);
}

/// The value for `key`, if the kernel gave one.
///
/// Before [`init`], the loader's copy: a shared library's constructors run
/// before the program's `__libc_start_main`, and some ask -- among them
/// compiler-builtins' choice of AArch64's LSE atomics, which without this
/// found no `AT_HWCAP` and kept the load-exclusive loops for every
/// dynamically linked program.
pub fn get(key: usize) -> Option<usize> {
    let mut at = AUXV.load(Ordering::Relaxed).cast_const();
    if at.is_null() {
        at = crate::loader::auxv()?;
    }
    loop {
        // SAFETY: `init`'s caller vouched for pairs ending in `AT_NULL`, and
        // `at` has not passed it.
        let found = unsafe { at.read() };
        if found == AT_NULL {
            return None;
        }
        // SAFETY: a key that is not `AT_NULL` is followed by its value.
        let value = unsafe { at.wrapping_add(1).read() };
        if found == key {
            return Some(value);
        }
        at = at.wrapping_add(2);
    }
}

/// The value for `key`, or zero with `errno` set to `ENOENT`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getauxval(key: c_ulong) -> c_ulong {
    // `c_ulong` and `usize` are both 64 bits on every target this builds for.
    match get(key as usize) {
        Some(value) => value as c_ulong,
        None => {
            errno::set(errno::ENOENT);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_found_by_key_until_at_null() {
        let vector = [AT_PAGESZ, 4096, AT_RANDOM, 0x1234, AT_NULL, 0, AT_PHNUM, 9];
        // SAFETY: the vector ends in `AT_NULL` and outlives every lookup below.
        unsafe { init(vector.as_ptr()) };

        assert_eq!(get(AT_PAGESZ), Some(4096));
        assert_eq!(get(AT_RANDOM), Some(0x1234));
        // Past `AT_NULL` is not part of the vector.
        assert_eq!(get(AT_PHNUM), None);

        assert_eq!(getauxval(AT_PAGESZ as c_ulong), 4096);
        errno::set(0);
        assert_eq!(getauxval(12345), 0);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::ENOENT);

        // SAFETY: null is never read.
        unsafe { init(core::ptr::null()) };
    }
}
