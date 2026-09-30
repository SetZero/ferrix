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

/// Allocates a memory protection key whose initial rights are
/// `access_rights` (`PKEY_DISABLE_ACCESS`, `PKEY_DISABLE_WRITE`). Returns the
/// key, or -1 with `errno` set: `ENOSPC` when every key is taken, and
/// `EINVAL` or `ENOSYS` where the processor or kernel has none, which is how
/// Chromium, the caller here, learns to go without.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pkey_alloc(flags: c_uint, access_rights: c_uint) -> c_int {
    // SAFETY: allocating a key reads and writes no user memory.
    let ret = unsafe { syscall::syscall2(nr::PKEY_ALLOC, flags as usize, access_rights as usize) };
    errno::from_syscall(ret) as c_int
}

/// Frees a key [`pkey_alloc`] gave.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pkey_free(key: c_int) -> c_int {
    // SAFETY: freeing a key reads and writes no user memory.
    let ret = unsafe { syscall::syscall2(nr::PKEY_FREE, key as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// [`mprotect`], also tagging the pages with the protection key `key`. As
/// in glibc, key -1 is plain `mprotect`, which a kernel without keys has too.
///
/// # Safety
///
/// As [`mprotect`]; the key's rights apply to the pages from now on.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pkey_mprotect(
    addr: *mut c_void,
    len: usize,
    prot: c_int,
    key: c_int,
) -> c_int {
    if key == -1 {
        // SAFETY: the caller's contract is `mprotect`'s.
        return unsafe { mprotect(addr, len, prot) };
    }
    // SAFETY: the caller vouches for the change.
    let ret = unsafe {
        syscall::syscall4(
            nr::PKEY_MPROTECT,
            addr.addr(),
            len,
            prot as usize,
            key as usize,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// The keys the protection key rights register has room for: two bits
/// each, access-disable and write-disable, in 32 bits.
#[cfg(target_arch = "x86_64")]
const PKEYS: c_int = 16;

/// The protection key rights register, `PKRU`, with `RDPKRU`.
///
/// Like glibc's, this does not ask the processor whether it has the
/// register: one without it faults. A program asks after a successful
/// [`pkey_alloc`], which there is none without it.
#[cfg(target_arch = "x86_64")]
fn read_pkru() -> u32 {
    let pkru: u32;
    // SAFETY: RDPKRU with ECX 0 reads the register into EAX, clears EDX,
    // and touches no memory.
    unsafe {
        core::arch::asm!(
            ".byte 0x0f, 0x01, 0xee",
            in("ecx") 0u32,
            out("eax") pkru,
            out("edx") _,
            options(nomem, nostack, preserves_flags),
        );
    }
    pkru
}

/// Writes the protection key rights register with `WRPKRU`.
#[cfg(target_arch = "x86_64")]
fn write_pkru(pkru: u32) {
    // SAFETY: WRPKRU with ECX and EDX 0 writes EAX to the register. It
    // changes which pages this thread may touch, which is what the caller
    // asked for, and nothing else.
    unsafe {
        core::arch::asm!(
            ".byte 0x0f, 0x01, 0xef",
            in("eax") pkru,
            in("ecx") 0u32,
            in("edx") 0u32,
            options(nostack, preserves_flags),
        );
    }
}

/// This thread's rights for `key`: `PKEY_DISABLE_ACCESS` and
/// `PKEY_DISABLE_WRITE`, from its two bits of `PKRU`. -1 with `EINVAL` for
/// a key outside 0 to 15.
///
/// On the other architectures -1 with `ENOSYS`, as glibc's generic
/// function answers; Arm's permission overlay is not read.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pkey_get(key: c_int) -> c_int {
    #[cfg(target_arch = "x86_64")]
    {
        if !(0..PKEYS).contains(&key) {
            errno::set(errno::EINVAL);
            return -1;
        }
        ((read_pkru() >> (2 * key)) & 3) as c_int
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = key;
        errno::set(errno::ENOSYS);
        -1
    }
}

/// Sets this thread's rights for `key` to `rights`, as [`pkey_get`] reads
/// them. -1 with `EINVAL` for a key outside 0 to 15 or rights above 3.
///
/// On the other architectures -1 with `ENOSYS`, as [`pkey_get`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pkey_set(key: c_int, rights: c_uint) -> c_int {
    #[cfg(target_arch = "x86_64")]
    {
        if !(0..PKEYS).contains(&key) || rights > 3 {
            errno::set(errno::EINVAL);
            return -1;
        }
        let shift = 2 * key;
        let pkru = (read_pkru() & !(3 << shift)) | (rights << shift);
        write_pkru(pkru);
        0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (key, rights);
        errno::set(errno::ENOSYS);
        -1
    }
}

/// `O_CLOEXEC`, which every architecture here numbers alike.
const O_CLOEXEC: c_int = 0o2_000_000;
/// `O_NONBLOCK`, likewise.
const O_NONBLOCK: c_int = 0o4000;

/// Opens the shared memory object `name`, a file in `/dev/shm`, with
/// `flags` and, when it is created, `mode`: musl's `shm_open`. The name is
/// mapped as a named semaphore's is ([`crate::semaphore::map_name`]).
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn shm_open(name: *const c_char, flags: c_int, mode: c_uint) -> c_int {
    let mut path = crate::semaphore::path_buffer();
    // SAFETY: the caller passes a NUL-terminated string.
    if let Err(error) = unsafe { crate::semaphore::map_name(name, &mut path) } {
        errno::set(error);
        return -1;
    }
    let old = crate::cancel::set_state(crate::cancel::DISABLE);
    let flags = flags | crate::fcntl::O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK;
    // SAFETY: `path` is NUL-terminated.
    let fd = unsafe { crate::fcntl::open(path.as_ptr().cast(), flags, mode) };
    let _ = crate::cancel::set_state(old);
    fd
}

/// Removes the shared memory object `name` ([`shm_open`]).
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn shm_unlink(name: *const c_char) -> c_int {
    let mut path = crate::semaphore::path_buffer();
    // SAFETY: the caller passes a NUL-terminated string.
    if let Err(error) = unsafe { crate::semaphore::map_name(name, &mut path) } {
        errno::set(error);
        return -1;
    }
    // SAFETY: `path` is NUL-terminated.
    unsafe { crate::unistd::unlink(path.as_ptr().cast()) }
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
