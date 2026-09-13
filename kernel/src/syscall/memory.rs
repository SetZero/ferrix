//! `mmap`, `munmap`, `mprotect`, `mremap` and `brk`.
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

use ferrix_bootinfo::{PAGE_SIZE, USER_VIRT_END};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    MAP_ANONYMOUS, MAP_FIXED, MAP_FIXED_NOREPLACE, MAP_PRIVATE, MAP_SHARED, MREMAP_FIXED,
    MREMAP_MAYMOVE, PROT_EXEC, PROT_GROWSDOWN, PROT_GROWSUP, PROT_READ, PROT_SEM, PROT_WRITE,
};
use ferrix_vma::VmaFlags;

use crate::syscall::process::Process;
use crate::user::space::{Destination, MMAP_MIN_ADDR, SpaceError};

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
    /// The file to map. Ignored for anonymous memory, as Linux ignores it.
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

    // Linux ignores an anonymous mapping's descriptor, and its offset once the
    // offset is whole pages, so a program passing a real fd with
    // `MAP_ANONYMOUS` gets zeroes there and must get them here. What Linux does
    // refuse is refused: a byte offset that is not a page boundary, which the
    // entry point checks before it looks at any flag, and an offset whose
    // pages would wrap.
    let _ = fd;
    let page_offset = match unit {
        OffsetUnit::Bytes if !offset.is_multiple_of(PAGE_SIZE) => return Err(Errno::EINVAL),
        OffsetUnit::Bytes => offset / PAGE_SIZE,
        OffsetUnit::Pages => offset,
    };
    let page_offset = usize::try_from(page_offset).map_err(|_| Errno::EOVERFLOW)?;
    let pages = usize::try_from(len / PAGE_SIZE).map_err(|_| Errno::ENOMEM)?;
    if page_offset.checked_add(pages).is_none() {
        return Err(Errno::EOVERFLOW);
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
    // Below the floor is `EPERM`, which is what Linux answers a process without
    // `CAP_SYS_RAWIO`. There are no capabilities here to hold, so that is every
    // process, root or not. Checked before the unmap below, so a refused call
    // changes nothing. A hint is not refused for the same address: the search
    // simply starts above the floor.
    if addr < MMAP_MIN_ADDR {
        return Err(Errno::EPERM);
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
///
/// The checks run in Linux's order, because the order decides which error a
/// bad call gets: both growth flags, then alignment, then a zero length --
/// which succeeds before `prot` is even looked at -- then wrapping, then the
/// protection bits. `PROT_SEM` is accepted and means nothing, as on Linux.
///
/// `PROT_GROWSDOWN` and `PROT_GROWSUP` extend the change to the start or end of
/// a region that grows. No region here grows -- the stack is a fixed
/// reservation -- and Linux answers either flag on a region that does not grow
/// with `EINVAL`, which is what they get.
pub(crate) fn sys_mprotect(
    process: &Process,
    addr: u64,
    len: u64,
    prot: u32,
) -> Result<usize, Errno> {
    const GROWS: u32 = PROT_GROWSDOWN | PROT_GROWSUP;
    if prot & GROWS == GROWS || !addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    if len == 0 {
        return Ok(0);
    }
    let len = pages_for(len)?;
    let vma = protection(prot & !(PROT_SEM | GROWS))?;
    if prot & GROWS != 0 {
        return Err(Errno::EINVAL);
    }
    process
        .space()
        .protect(addr, len, vma)
        .map_err(|error| match error {
            // A range that is not wholly mapped, or runs out of the user half,
            // is `ENOMEM` from `mprotect`, not `EINVAL`.
            SpaceError::NotUserRange(_) | SpaceError::BadRange => Errno::ENOMEM,
            other => refused(other),
        })?;
    Ok(0)
}

/// `mremap`.
///
/// What glibc's `realloc` does with a block too large for its heap, so what
/// matters most is that the contents arrive: a block that moved and came
/// back zeroed is a program corrupted far from here. The address space moves
/// the pages rather than copying them; see [`crate::user::space::AddressSpace::remap`].
///
/// # What is refused, and in what order
///
/// Linux's order, from `mm/mremap.c`: unknown flags, then `MREMAP_FIXED`
/// without `MREMAP_MAYMOVE`, then an unaligned old address, then a new length
/// of zero -- all `EINVAL`. `MREMAP_DONTUNMAP` is among the unknown flags,
/// which is what a kernel older than 5.7 answers and what a program must
/// already handle.
///
/// An old length of zero is `EINVAL` too. On Linux it asks for a second
/// mapping of the same pages of a *shared* mapping, which nothing here can
/// make yet; refusing is what Linux does for a private one.
///
/// A fixed destination is then `EINVAL` if unaligned, past the top of user
/// space, or overlapping the old range; an old range that is not mapped is
/// `EFAULT`; and only after both is a destination below `MMAP_MIN_ADDR`
/// `EPERM`, where Linux's `get_unmapped_area` calls `security_mmap_addr`.
pub(crate) fn sys_mremap(
    process: &Process,
    old_addr: u64,
    old_size: u64,
    new_size: u64,
    flags: u32,
    new_addr: u64,
) -> Result<usize, Errno> {
    if flags & !(MREMAP_MAYMOVE | MREMAP_FIXED) != 0 {
        return Err(Errno::EINVAL);
    }
    if flags & MREMAP_FIXED != 0 && flags & MREMAP_MAYMOVE == 0 {
        return Err(Errno::EINVAL);
    }
    if !old_addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    // A length that wraps when rounded is as unusable as zero, and Linux's
    // `PAGE_ALIGN` makes it zero.
    let old_len = pages_for(old_size).map_err(|_| Errno::EINVAL)?;
    let new_len = pages_for(new_size).map_err(|_| Errno::EINVAL)?;

    let destination = if flags & MREMAP_FIXED != 0 {
        if !new_addr.is_multiple_of(PAGE_SIZE) {
            return Err(Errno::EINVAL);
        }
        // `check_mremap_params` goes on to refuse a destination running past
        // the top of user space, then one overlapping the old range: both
        // `EINVAL`, and both before any mapping is looked at.
        let new_end = new_addr
            .checked_add(new_len)
            .filter(|&end| end <= USER_VIRT_END)
            .ok_or(Errno::EINVAL)?;
        if new_addr < old_addr.saturating_add(old_len) && old_addr < new_end {
            return Err(Errno::EINVAL);
        }
        // Below the floor is `EPERM`, as from `mmap`, but later in the call.
        // Linux looks up the old mapping (`EFAULT`), unmaps the destination,
        // and only then reaches `get_unmapped_area`, whose
        // `security_mmap_addr` refuses the address. The old mapping is looked
        // for first here too; the destination is left alone, so a refused
        // call changes nothing.
        if new_addr < MMAP_MIN_ADDR {
            let old_end = old_addr.saturating_add(old_len);
            let mapped = process
                .space()
                .regions()
                .iter()
                .any(|region| region.start <= old_addr && old_end <= region.end);
            return Err(if mapped { Errno::EPERM } else { Errno::EFAULT });
        }
        Destination::Fixed(new_addr)
    } else if flags & MREMAP_MAYMOVE != 0 {
        Destination::Anywhere
    } else {
        Destination::InPlace
    };
    process
        .space()
        .remap(old_addr, old_len, new_len, destination)
        .map(usize_of)
        .map_err(refused)
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
