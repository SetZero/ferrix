//! Reading and writing a user program's memory.
//!
//! Every system call that takes a pointer goes through here. That is the point
//! of the module: there is exactly one place where the kernel touches memory a
//! program chose the address of, so there is exactly one place to get the
//! checks right and one place to change when the hardware starts helping.
//!
//! # Why not just dereference it
//!
//! Three reasons, and each of them is a bug that has been shipped by somebody.
//!
//! **The address may not be mapped yet.** A user page is reserved by `mmap`
//! and paid for on first touch. The program may have written to a buffer, or
//! may only have reserved it; either way the kernel is often the first to
//! touch a given page. Dereferencing would fault in the kernel, on a kernel
//! stack, at a point where the fault handler cannot tell a legitimate
//! demand-paged access from a wild pointer. So the copy asks
//! [`AddressSpace::fault`] first, deliberately, and a page that cannot be
//! faulted in is [`UserError::Fault`] — a clean `EFAULT` to the program rather
//! than a kernel fault.
//!
//! **The address may not be the program's.** Nothing stops a program passing
//! a kernel address as a buffer. On this tree nothing in the *hardware* stops
//! the kernel following it either: no SMAP on x86-64, no PAN on Arm, not yet.
//! The bound check below is therefore the only thing between a user pointer
//! and a read of kernel memory at kernel privilege, which is why it is the
//! first statement in both functions and why it happens before any arithmetic
//! that could wrap.
//!
//! **The program's tables are not the kernel's.** A user address means nothing
//! in the kernel's own translation until the space is installed on this
//! processor, and even then only for the lower half. The copy resolves through
//! the target [`AddressSpace`] explicitly and reaches the page through the
//! direct map, so it works on a space that is *not* installed anywhere — which
//! is what `execve` needs when it writes an argument vector into a space the
//! processor has not switched to yet.
//!
//! # Page at a time, and why the bound is checked twice
//!
//! A user buffer is contiguous in the program's address space and need not be
//! contiguous in physical memory, so the copy is split at page boundaries and
//! each page resolved separately. The end of the range is bound-checked before
//! the loop *and* each page is checked as it is reached, because the first
//! check proves the caller asked for something sensible and the second proves
//! the walk stayed inside it.

use ferrix_bootinfo::{PAGE_SIZE, is_user_address};

use crate::mm;
use crate::user::space::{Access, AddressSpace, SpaceError};

/// Why a copy to or from user memory failed.
///
/// Deliberately coarse. A program learns only `EFAULT`: telling it *which* of
/// its pages was unmapped is telling it about the kernel's address space, and
/// the distinctions matter to the kernel's own log rather than to the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserError {
    /// The address, or the end of the range, is not a user address.
    NotUserRange,
    /// The range is so long that its end cannot be computed.
    Overflow,
    /// The page is not mapped, is not writable, or could not be faulted in.
    Fault,
}

impl From<SpaceError> for UserError {
    /// Everything the address space can refuse is `EFAULT` to a program.
    fn from(_: SpaceError) -> Self {
        UserError::Fault
    }
}

/// Check that `[at, at + len)` lies wholly in the half a program gets.
///
/// Separated out and called first by both directions, because the ordering is
/// the security property: a length that wraps must be refused *before* it is
/// added to anything, or the addition is the bug.
fn check_range(at: u64, len: u64) -> Result<(), UserError> {
    if len == 0 {
        return Ok(());
    }
    if !is_user_address(at) {
        return Err(UserError::NotUserRange);
    }
    // `len - 1`, not `len`: a buffer ending exactly at the top of the user
    // half is legal, and checking the first byte past it would refuse it.
    let last = at
        .checked_add(len.wrapping_sub(1))
        .ok_or(UserError::Overflow)?;
    if !is_user_address(last) {
        return Err(UserError::NotUserRange);
    }
    Ok(())
}

/// How much of a page is left from `at`.
fn to_page_end(at: u64) -> u64 {
    PAGE_SIZE - (at % PAGE_SIZE)
}

/// Resolve one user address to somewhere the kernel can touch it.
///
/// Faults the page in first, then translates through the space's own tables
/// and returns the direct-map address of the byte. `access` decides both what
/// the fault is allowed to do and, through it, whether a copy-on-write page is
/// copied before the kernel writes to it — which is why a write must not be
/// resolved with [`Access::READ`].
fn resolve(space: &AddressSpace, at: u64, access: Access) -> Result<u64, UserError> {
    if !is_user_address(at) {
        return Err(UserError::NotUserRange);
    }
    space.fault(at, access)?;
    let root = space.root_table();
    let phys = mm::translate_in(root, at).ok_or(UserError::Fault)?;
    Ok(mm::direct_map(phys))
}

/// Copy `out.len()` bytes out of the program's memory at `from`.
///
/// # Errors
///
/// [`UserError`]. Nothing here panics, and nothing is copied at all if the
/// range check fails.
pub(crate) fn copy_from_user(
    space: &AddressSpace,
    from: u64,
    out: &mut [u8],
) -> Result<(), UserError> {
    let len = u64::try_from(out.len()).map_err(|_| UserError::Overflow)?;
    check_range(from, len)?;

    let mut done = 0_usize;
    while done < out.len() {
        let at = from
            .checked_add(u64::try_from(done).map_err(|_| UserError::Overflow)?)
            .ok_or(UserError::Overflow)?;
        let chunk = chunk_len(at, out.len() - done)?;
        let source = resolve(space, at, Access::READ)?;

        let target = out.get_mut(done..done + chunk).ok_or(UserError::Overflow)?;
        // SAFETY: `resolve` faulted the page in and translated it through the
        // space's own tables, so `source` is the direct-map address of a live
        // frame; `chunk` was clamped to the remainder of that page, so the
        // whole read is inside it. The direct map is readable for all of RAM.
        let bytes = unsafe { core::slice::from_raw_parts(source as *const u8, chunk) };
        target.copy_from_slice(bytes);
        done += chunk;
    }
    Ok(())
}

/// Copy `data` into the program's memory at `to`.
///
/// # Errors
///
/// [`UserError`]. A partial copy is possible if a later page cannot be
/// resolved, which matches Linux: `write` and its kin report the count they
/// managed, and a caller that needs all-or-nothing must check the range first.
pub(crate) fn copy_to_user(space: &AddressSpace, to: u64, data: &[u8]) -> Result<(), UserError> {
    let len = u64::try_from(data.len()).map_err(|_| UserError::Overflow)?;
    check_range(to, len)?;

    let mut done = 0_usize;
    while done < data.len() {
        let at = to
            .checked_add(u64::try_from(done).map_err(|_| UserError::Overflow)?)
            .ok_or(UserError::Overflow)?;
        let chunk = chunk_len(at, data.len() - done)?;
        // `Access::WRITE`, which is what copies a copy-on-write page before
        // the kernel writes into it. Resolving with `READ` here would have the
        // kernel writing into a page the parent can still see.
        let target = resolve(space, at, Access::WRITE)?;

        let source = data.get(done..done + chunk).ok_or(UserError::Overflow)?;
        // SAFETY: `resolve` faulted the page in for writing and translated it
        // through the space's own tables, so `target` is the direct-map
        // address of a live frame this space may write; `chunk` was clamped to
        // the remainder of that page. The direct map is writable for RAM.
        let bytes = unsafe { core::slice::from_raw_parts_mut(target as *mut u8, chunk) };
        bytes.copy_from_slice(source);
        done += chunk;
    }
    Ok(())
}

/// How many bytes of `remaining` may be copied starting at `at` without
/// leaving the page `at` is in.
fn chunk_len(at: u64, remaining: usize) -> Result<usize, UserError> {
    let to_end = usize::try_from(to_page_end(at)).map_err(|_| UserError::Overflow)?;
    Ok(to_end.min(remaining))
}

/// Read a NUL-terminated string out of the program's memory.
///
/// `limit` bounds the answer, because a program can pass a pointer into a
/// region with no NUL in it and the kernel must not walk to the end of the
/// address space looking for one. Linux calls that bound `PATH_MAX` or
/// `MAX_ARG_STRLEN` depending on the caller; the caller passes it here.
///
/// Returns the bytes without the terminator.
///
/// # Errors
///
/// [`UserError::Fault`] if the string is unterminated within `limit`, which is
/// what Linux reports for a path that long — the program gets `EFAULT` rather
/// than a truncated string it did not ask for.
pub(crate) fn copy_cstr_from_user(
    space: &AddressSpace,
    from: u64,
    limit: usize,
    out: &mut alloc::vec::Vec<u8>,
) -> Result<(), UserError> {
    out.clear();
    let mut at = from;
    while out.len() < limit {
        // One page at a time, so that a string near the top of a mapping does
        // not require the *next* page to be mapped at all.
        let source = resolve(space, at, Access::READ)?;
        let span = usize::try_from(to_page_end(at)).map_err(|_| UserError::Overflow)?;
        let span = span.min(limit - out.len());
        // SAFETY: as `copy_from_user`. `span` stays inside the resolved page.
        let bytes = unsafe { core::slice::from_raw_parts(source as *const u8, span) };
        match bytes.iter().position(|&b| b == 0) {
            Some(end) => {
                out.extend_from_slice(bytes.get(..end).ok_or(UserError::Overflow)?);
                return Ok(());
            }
            None => out.extend_from_slice(bytes),
        }
        at = at
            .checked_add(u64::try_from(span).map_err(|_| UserError::Overflow)?)
            .ok_or(UserError::Overflow)?;
    }
    Err(UserError::Fault)
}
