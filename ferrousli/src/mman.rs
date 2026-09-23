//! `sys/mman.h`: mapping memory, and changing and locking what is mapped.
//!
//! `mremap` is variadic in C; [`crate::fcntl`] says why it is defined with a
//! fixed fifth parameter.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::ptr::with_exposed_provenance_mut;

use crate::errno;
use crate::syscall::{self, nr};

/// `MAP_FAILED`: what `mmap` and `mremap` return on failure.
pub const MAP_FAILED: *mut c_void = with_exposed_provenance_mut(usize::MAX);

/// `MAP_FIXED`, from `asm-generic/mman-common.h`.
const MAP_FIXED: c_int = 0x10;
/// `MAP_ANONYMOUS`, from `asm-generic/mman-common.h`.
const MAP_ANONYMOUS: c_int = 0x20;
/// `MREMAP_FIXED`, from `linux/mman.h`.
const MREMAP_FIXED: c_int = 2;
/// `POSIX_MADV_DONTNEED`, from musl's `sys/mman.h`.
const POSIX_MADV_DONTNEED: c_int = 4;

/// A mapping's address from the kernel's return value, or `MAP_FAILED` with
/// `errno` set.
fn mapping(ret: isize) -> *mut c_void {
    match errno::decode(ret) {
        Ok(address) => with_exposed_provenance_mut(address),
        Err(error) => {
            errno::set(error);
            MAP_FAILED
        }
    }
}

/// Maps `len` bytes of `fd` from `offset`, or anonymous memory.
///
/// Adapted from musl (MIT): a length the address space cannot hold fails
/// with `ENOMEM` before the kernel sees it, and the kernel's `EPERM` for an
/// anonymous mapping that did not ask for an address, which it gives when
/// `vm.mmap_min_addr` forbids a low one, is reported as the `ENOMEM` POSIX
/// specifies.
///
/// # Safety
///
/// A `MAP_FIXED` mapping replaces whatever was at `addr`, which must not be
/// memory anything still uses.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mmap(
    addr: *mut c_void,
    len: usize,
    prot: c_int,
    flags: c_int,
    fd: c_int,
    offset: i64,
) -> *mut c_void {
    if len >= isize::MAX as usize {
        errno::set(errno::ENOMEM);
        return MAP_FAILED;
    }
    // SAFETY: the caller vouches for replacing a fixed address; otherwise the
    // kernel picks memory nothing uses.
    let ret = unsafe {
        syscall::mmap(
            addr.addr(),
            len,
            prot as usize,
            flags as usize,
            fd as usize,
            offset,
        )
    };
    if ret == -(errno::EPERM as isize)
        && addr.is_null()
        && flags & MAP_ANONYMOUS != 0
        && flags & MAP_FIXED == 0
    {
        errno::set(errno::ENOMEM);
        return MAP_FAILED;
    }
    mapping(ret)
}

/// glibc's large-file name for [`mmap`].
///
/// # Safety
///
/// As [`mmap`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mmap64(
    addr: *mut c_void,
    len: usize,
    prot: c_int,
    flags: c_int,
    fd: c_int,
    offset: i64,
) -> *mut c_void {
    // SAFETY: the caller's contract is `mmap`'s.
    unsafe { mmap(addr, len, prot, flags, fd, offset) }
}

/// Unmaps the pages in `len` bytes from `addr`.
///
/// # Safety
///
/// Nothing may use the memory afterwards.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn munmap(addr: *mut c_void, len: usize) -> c_int {
    // SAFETY: the caller vouches that the memory is no longer used.
    let ret = unsafe { syscall::syscall2(nr::MUNMAP, addr.addr(), len) };
    errno::from_syscall(ret) as c_int
}

/// Sets the protection of the pages in `len` bytes from `addr`.
///
/// # Safety
///
/// Code that reads, writes or runs the memory must expect the new
/// protection.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int {
    // SAFETY: the caller vouches for the change.
    let ret = unsafe { syscall::syscall3(nr::MPROTECT, addr.addr(), len, prot as usize) };
    errno::from_syscall(ret) as c_int
}

/// Writes changes in a shared file mapping back to the file.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn msync(addr: *mut c_void, len: usize, flags: c_int) -> c_int {
    // SAFETY: `msync` changes no mapping and reads no user memory.
    let ret =
        unsafe { crate::cancel::syscall_cp(nr::MSYNC, addr.addr(), len, flags as usize, 0, 0, 0) };
    errno::from_syscall(ret) as c_int
}

/// Advises the kernel how memory will be used.
///
/// # Safety
///
/// Some advice changes contents: `MADV_DONTNEED` and `MADV_FREE` discard
/// them, and nothing may rely on them afterwards.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int {
    // SAFETY: the caller vouches for the advice's effect.
    let ret = unsafe { syscall::syscall3(nr::MADVISE, addr.addr(), len, advice as usize) };
    errno::from_syscall(ret) as c_int
}

/// POSIX's advice about memory use. Returns the error number rather than
/// setting `errno`.
///
/// `POSIX_MADV_DONTNEED` must not discard contents, and Linux's
/// `MADV_DONTNEED`, which has the same number, does. So it is accepted and
/// ignored, as musl does.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn posix_madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int {
    if advice == POSIX_MADV_DONTNEED {
        return 0;
    }
    // POSIX defines five values, 0 to 4, and refuses any other; the kernel
    // would take Linux's own advice, and an emulator takes anything.
    if !(0..POSIX_MADV_DONTNEED).contains(&advice) {
        return errno::EINVAL;
    }
    // SAFETY: the remaining POSIX advice values are hints that change no
    // contents.
    let ret = unsafe { syscall::syscall3(nr::MADVISE, addr.addr(), len, advice as usize) };
    match errno::decode(ret) {
        Ok(_) => 0,
        Err(error) => error,
    }
}

/// Grows, shrinks or moves a mapping. `new_addr` is read only with
/// `MREMAP_FIXED`.
///
/// # Safety
///
/// With `MREMAP_MAYMOVE`, nothing may use the old address afterwards. With
/// `MREMAP_FIXED`, whatever was at `new_addr` is replaced.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mremap(
    old_addr: *mut c_void,
    old_len: usize,
    new_len: usize,
    flags: c_int,
    new_addr: *mut c_void,
) -> *mut c_void {
    if new_len >= isize::MAX as usize {
        errno::set(errno::ENOMEM);
        return MAP_FAILED;
    }
    // The caller passes the fifth argument only with `MREMAP_FIXED`, and the
    // kernel reads it as a hint with `MREMAP_DONTUNMAP`.
    let new_addr = if flags & MREMAP_FIXED != 0 {
        new_addr.addr()
    } else {
        0
    };
    // SAFETY: the caller vouches for the old and new addresses.
    let ret = unsafe {
        syscall::syscall6(
            nr::MREMAP,
            old_addr.addr(),
            old_len,
            new_len,
            flags as usize,
            new_addr,
            0,
        )
    };
    mapping(ret)
}

/// Locks the pages in `len` bytes from `addr` into memory.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn mlock(addr: *const c_void, len: usize) -> c_int {
    // SAFETY: locking changes no contents or protection.
    let ret = unsafe { syscall::syscall2(nr::MLOCK, addr.addr(), len) };
    errno::from_syscall(ret) as c_int
}

/// Unlocks the pages in `len` bytes from `addr`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn munlock(addr: *const c_void, len: usize) -> c_int {
    // SAFETY: unlocking changes no contents or protection.
    let ret = unsafe { syscall::syscall2(nr::MUNLOCK, addr.addr(), len) };
    errno::from_syscall(ret) as c_int
}

/// Locks every page of the process into memory.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn mlockall(flags: c_int) -> c_int {
    // SAFETY: as `mlock`. The second argument is unused.
    let ret = unsafe { syscall::syscall2(nr::MLOCKALL, flags as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// Unlocks every page of the process.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn munlockall() -> c_int {
    // SAFETY: as `munlock`.
    let ret = unsafe { syscall::syscall0(nr::MUNLOCKALL) };
    errno::from_syscall(ret) as c_int
}

/// Creates an anonymous file that lives in memory, and opens it.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memfd_create(name: *const c_char, flags: c_uint) -> c_int {
    // SAFETY: the kernel only reads `name`.
    let ret = unsafe { syscall::syscall2(nr::MEMFD_CREATE, name.addr(), flags as usize) };
    errno::from_syscall(ret) as c_int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_mapping_is_map_failed_with_errno() {
        assert_eq!(mapping(-(errno::ENOMEM as isize)), MAP_FAILED);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::ENOMEM);
        assert_eq!(mapping(0x1000).addr(), 0x1000);
    }

    #[test]
    fn an_impossible_length_fails_before_the_kernel() {
        // SAFETY: the call fails before mapping anything.
        let ret = unsafe { mmap(core::ptr::null_mut(), usize::MAX, 0, MAP_ANONYMOUS, -1, 0) };
        assert_eq!(ret, MAP_FAILED);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::ENOMEM);
    }
}
