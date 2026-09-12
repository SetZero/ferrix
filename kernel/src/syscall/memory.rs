//! `mmap`, `munmap`, `mprotect` and `brk`.
//!
//! The four calls a program reshapes its own address space with, and the first
//! four a static musl binary makes: it allocates with `mmap` before it does
//! anything else, and `brk` only as a fallback. (A traced static *glibc*
//! binary is the other way round, which is worth knowing because planning from
//! a glibc trace would have these built in the wrong order.)
//!
//! # Where the work actually is
//!
//! Not here. `libs/vma` already implements the interval tree and the three
//! operations that reshape it — insert, remove and protect, with splitting and
//! merging, host-tested — and `AddressSpace` already turns a region into pages
//! on demand. What is left in this module is argument decoding, which sounds
//! trivial and is where the bugs are: a length that wraps when rounded up, an
//! offset counted in the wrong unit, a `PROT_NONE` that is silently turned
//! into a readable page.

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    MAP_ANONYMOUS, MAP_FIXED, MAP_FIXED_NOREPLACE, MAP_PRIVATE, MAP_SHARED, PROT_EXEC, PROT_READ,
    PROT_WRITE,
};
use ferrix_vma::VmaFlags;

use crate::syscall::process::Process;
use crate::user::space::SpaceError;

/// What `mmap`'s sixth argument is counted in.
///
/// The reason [`super::Syscall::Mmap2`] is a different call rather than a
/// different number for the same one. Getting this wrong maps the wrong part
/// of a file, and silently: every address is valid, just not the one asked
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OffsetUnit {
    /// `mmap`: bytes.
    Bytes,
    /// `mmap2`, ARMv7-A only: 4096-byte units, because a 32-bit register
    /// cannot carry a file offset in bytes.
    Pages,
}

/// Turn `PROT_*` into region flags.
///
/// `PROT_NONE` is zero and means no access at all, which [`VmaFlags::NONE`]
/// says exactly. It is a real request — libc reserves guard pages with it —
/// and must not be quietly widened to readable.
fn protection(prot: u32) -> Result<VmaFlags, Errno> {
    const KNOWN: u32 = PROT_READ | PROT_WRITE | PROT_EXEC;
    if prot & !KNOWN != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(VmaFlags {
        read: prot & PROT_READ != 0,
        write: prot & PROT_WRITE != 0,
        execute: prot & PROT_EXEC != 0,
        ..VmaFlags::NONE
    })
}

/// Everything an address space can refuse, as the program sees it.
fn refused(error: SpaceError) -> Errno {
    match error {
        SpaceError::OutOfMemory | SpaceError::Backing(_) => Errno::ENOMEM,
        SpaceError::NotUserRange(_) | SpaceError::BadRange => Errno::EINVAL,
        SpaceError::NotMapped(_) | SpaceError::Refused(_) => Errno::EFAULT,
    }
}

/// Round a length up to a whole number of pages.
///
/// A length of zero is `EINVAL` for `mmap`, and the rounding must not wrap:
/// `mmap(NULL, usize::MAX, ...)` is a thing programs do by accident and it
/// must be an error, not a very small mapping.
fn pages_for(len: u64) -> Result<u64, Errno> {
    if len == 0 {
        return Err(Errno::EINVAL);
    }
    len.checked_add(PAGE_SIZE - 1)
        .map(|len| len & !(PAGE_SIZE - 1))
        .ok_or(Errno::ENOMEM)
}

/// `mmap`'s arguments, as the ABI passes them.
///
/// A struct rather than seven parameters because seven positional arguments of
/// which three are integers is a call nobody can read, and because the trap
/// path hands them over as a block anyway.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MmapRequest {
    /// Where the program wants it; a hint unless `MAP_FIXED` is set.
    pub(crate) addr: u64,
    /// How many bytes, before rounding.
    pub(crate) len: u64,
    /// `PROT_*`.
    pub(crate) prot: u32,
    /// `MAP_*`.
    pub(crate) flags: u32,
    /// The file to map, or `-1` for anonymous memory.
    pub(crate) fd: i64,
    /// The offset into that file, in `unit`s.
    pub(crate) offset: u64,
    /// What `offset` is counted in.
    pub(crate) unit: OffsetUnit,
}

/// `mmap` and `mmap2`.
///
/// Anonymous memory only. A file-backed mapping needs the VFS, which is stage
/// 8; asking for one is `ENODEV` rather than a mapping of zeroes, because
/// zeroes where a file's contents should be is the kind of wrong that shows up
/// a long way from here.
pub(crate) fn sys_mmap(process: &Process, request: &MmapRequest) -> Result<usize, Errno> {
    let &MmapRequest {
        addr,
        len,
        prot,
        flags,
        fd,
        offset,
        unit,
    } = request;
    let mut vma = protection(prot)?;
    let len = pages_for(len)?;

    if flags & MAP_ANONYMOUS == 0 {
        return Err(Errno::ENODEV);
    }
    // Exactly one of SHARED and PRIVATE, which is what Linux requires.
    let shared = flags & MAP_SHARED != 0;
    if shared == (flags & MAP_PRIVATE != 0) {
        return Err(Errno::EINVAL);
    }
    vma.shared = shared;

    // An anonymous mapping's fd must be -1 and its offset zero. Checking is
    // not pedantry: a program passing a real fd here meant `MAP_ANONYMOUS` to
    // be absent, and succeeding would hand it zeroes.
    if fd != -1 {
        return Err(Errno::EINVAL);
    }
    let offset = match unit {
        OffsetUnit::Bytes => offset,
        OffsetUnit::Pages => offset.checked_mul(PAGE_SIZE).ok_or(Errno::EINVAL)?,
    };
    if offset != 0 {
        return Err(Errno::EINVAL);
    }

    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    if !fixed {
        // A non-null address without MAP_FIXED is a hint, and a hint that does
        // not fit is answered elsewhere rather than refused.
        let hint = (addr != 0).then_some(addr);
        return process
            .space()
            .map_anywhere(hint, len, vma)
            .map(usize_of)
            .map_err(refused);
    }

    if !addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    if flags & MAP_FIXED_NOREPLACE == 0 {
        // Plain MAP_FIXED replaces whatever is there. Unmapping first is what
        // makes that true; an unmap of a range holding nothing is not an
        // error here, because the program asked for the result, not the steps.
        let _ = process.space().unmap(addr, len);
    }
    process
        .space()
        .map_anonymous(addr, len, vma)
        .map(|_| usize_of(addr))
        .map_err(refused)
}

/// `munmap`.
///
/// Unmapping a range that is only partly mapped is not an error: Linux removes
/// what is there and succeeds, and a libc freeing an arena relies on it.
pub(crate) fn sys_munmap(process: &Process, addr: u64, len: u64) -> Result<usize, Errno> {
    if !addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    let len = pages_for(len)?;
    process.space().unmap(addr, len).map_err(refused)?;
    Ok(0)
}

/// `mprotect`.
///
/// Load-bearing, and one of only three calls a static binary cannot survive
/// losing: a libc applies `RELRO` with it after relocation and aborts if that
/// fails.
pub(crate) fn sys_mprotect(
    process: &Process,
    addr: u64,
    len: u64,
    prot: u32,
) -> Result<usize, Errno> {
    if !addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    let vma = protection(prot)?;
    let len = pages_for(len)?;
    process.space().protect(addr, len, vma).map_err(refused)?;
    Ok(0)
}

/// `brk`.
///
/// Returns the break rather than an error, always — see
/// [`Process::set_break`] for why reporting `-ENOMEM` here would be worse than
/// useless.
pub(crate) fn sys_brk(process: &Process, want: u64) -> Result<usize, Errno> {
    Ok(usize_of(process.set_break(want)))
}

/// An address as the return register carries it.
///
/// The cast cannot lose data on any target Ferrix builds for: a user address
/// is below `USER_VIRT_END`, which is at most the pointer width.
fn usize_of(address: u64) -> usize {
    usize::try_from(address).unwrap_or(usize::MAX)
}
