//! Turning an ELF image into an address space a program can run in.
//!
//! `libs/elf` already parses, validates and is fuzzed; what it deliberately
//! does not do is touch memory. This is the half that does: it takes a parsed
//! image and a fresh [`AddressSpace`] and leaves behind the mappings, the
//! entry point, and the three numbers the auxiliary vector needs to tell the
//! program where its own program headers are.
//!
//! # Map first, copy second
//!
//! The pages arrive on fault, so a region has to exist before anything can be
//! written into it. That ordering also gives `.bss` for free: a committed
//! anonymous page is already zeroed, so the excess of `p_memsz` over
//! `p_filesz` needs nothing done to it. Copying zeroes over it would be
//! slower and would commit pages the program may never touch.
//!
//! # Why the permissions are computed per page and not per segment
//!
//! Because segments do not have to start on page boundaries, and two of them
//! can share one. Mapping each segment separately would then either overlap —
//! which the region map refuses, correctly — or silently give the shared page
//! one segment's permissions and not the other's.
//!
//! So the span is mapped once, writable, everything is copied in, and then the
//! permissions are applied over runs of pages that agree. A page covered by
//! two segments gets the union of their permissions, which is the only answer
//! that lets both segments work.
//!
//! And if that union comes out writable *and* executable, the image is
//! refused. Ferrix sweeps its own mappings for W^X at boot and would be
//! building one here on purpose otherwise. It does not happen for a binary
//! linked in this decade — `ld -z separate-code` has been the default for
//! years, precisely so that text and data never share a page — so refusing
//! costs nothing and names the problem if it ever appears.
//!
//! # A static PIE is placed, not relocated
//!
//! rustc links a musl program as a static position-independent executable
//! unless told otherwise: `ET_DYN`, no interpreter, and a self-relocating
//! start (musl's `rcrt1.o`). Linux maps such an image at a base of its own
//! choosing and applies no relocation; the program finds its base from
//! `AT_PHDR` less the program headers' link address and relocates itself before
//! anything reads an absolute address. So the loader does the same: every
//! address the image names is moved by [`PIE_BASE`] less the image's lowest
//! page, the entry, `AT_PHDR` and the heap's start with it, and `AT_BASE`, the
//! interpreter's base, stays zero because there is none. An `ET_DYN` image
//! that names an interpreter is a dynamically linked program, which needs a
//! dynamic linker this kernel does not have, and is refused by name.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::{PAGE_SIZE, USER_VIRT_END, is_user_address};
use ferrix_elf::{Elf, ElfError, PF_R, PF_W, PF_X, PT_INTERP, Segment};
use ferrix_vma::VmaFlags;

use crate::arch;
use crate::syscall::uaccess::{self, UserError};
use crate::user::space::{AddressSpace, SpaceError};

/// Where a static PIE's lowest page is placed: two thirds of the way up the
/// user half, page-aligned, as Linux's `ELF_ET_DYN_BASE` puts an `ET_DYN` image
/// on every architecture. Far above where a fixed-address program is linked,
/// and far below the stack and the mappings that grow down from it.
pub(crate) const PIE_BASE: u64 = (USER_VIRT_END / 3 * 2) & !(PAGE_SIZE - 1);

/// What the loader learned, and the program needs to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Loaded {
    /// Where execution starts.
    pub(crate) entry: u64,
    /// `AT_PHDR`: where the program headers ended up in memory. Zero if no
    /// loadable segment covers them, which a static binary's do.
    pub(crate) phdr: u64,
    /// `AT_PHENT`: the size of one program header.
    pub(crate) phent: u64,
    /// `AT_PHNUM`: how many there are.
    pub(crate) phnum: u64,
    /// One past the highest address the image occupies, which is where the
    /// heap goes.
    pub(crate) end: u64,
}

/// Why an image could not be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadError {
    /// `libs/elf` refused it.
    Malformed(ElfError),
    /// Built for another architecture.
    WrongMachine(u16),
    /// Position-independent and naming an interpreter: a dynamically linked
    /// program, which needs a dynamic linker to load it.
    NeedsInterpreter,
    /// It has no loadable segments at all.
    Empty,
    /// A page would have to be both writable and executable.
    WriteExecute(u64),
    /// The entry point is not a user address.
    EntryNotUser(u64),
    /// The address space refused a mapping.
    Space(SpaceError),
    /// A segment's contents could not be written into the space.
    Copy(UserError),
}

impl From<SpaceError> for LoadError {
    fn from(error: SpaceError) -> Self {
        LoadError::Space(error)
    }
}

impl From<UserError> for LoadError {
    fn from(error: UserError) -> Self {
        LoadError::Copy(error)
    }
}

/// Refuse what [`load`] would refuse about the image itself, without touching
/// any memory.
///
/// For `execve`, which has to decide whether the call can still fail before it
/// takes the running program's memory away. Everything checked here is a
/// property of the bytes; what is left for [`load`] to find -- a page that
/// would be writable and executable, a mapping the space refuses -- is found
/// after the point of no return, and kills the process as it does on Linux.
///
/// # Errors
///
/// The [`LoadError`] [`load`] would return for the same image.
pub(crate) fn check(image: &[u8]) -> Result<(), LoadError> {
    let elf = Elf::parse(image).map_err(LoadError::Malformed)?;
    elf.check_machine(arch::ARCH.elf_machine())
        .map_err(|_| LoadError::WrongMachine(elf.machine()))?;
    elf.validate_segments().map_err(LoadError::Malformed)?;
    let (low, _) = elf.load_span(PAGE_SIZE).ok_or(LoadError::Empty)?;
    let bias = bias_of(&elf, low)?;
    let _ = entry_point(&elf, bias)?;
    Ok(())
}

/// How far the image is moved from where it is linked: zero for a
/// fixed-address image, and from its lowest page `low` to [`PIE_BASE`] for a
/// static PIE.
fn bias_of(elf: &Elf<'_>, low: u64) -> Result<u64, LoadError> {
    if !elf.is_pie() {
        return Ok(0);
    }
    if elf.segments().any(|segment| segment.kind == PT_INTERP) {
        return Err(LoadError::NeedsInterpreter);
    }
    Ok(PIE_BASE.wrapping_sub(low))
}

/// Where execution starts, if a program may run there.
///
/// An entry point outside the user half is refused as a property of the image,
/// before anything is mapped, because entering it does not merely fault in the
/// program. x86-64 enters user mode with `sysretq`, which takes the address in
/// `RCX` and raises `#GP` *in ring 0* when that address is not canonical, so
/// the kernel would take the program's mistake as its own fault.
///
/// On ARMv7-A bit 0 of the entry point says Thumb, and the instruction is at
/// the address with that bit clear. The test needs no mask for it: the bound is
/// even, so clearing bit 0 cannot move an address from one side of it to the
/// other, and the answer is the same with the bit or without it.
fn entry_point(elf: &Elf<'_>, bias: u64) -> Result<u64, LoadError> {
    const _: () = assert!(
        USER_VIRT_END.is_multiple_of(2),
        "the Thumb bit could cross the bound"
    );
    let entry = elf.entry().wrapping_add(bias);
    if !is_user_address(entry) {
        return Err(LoadError::EntryNotUser(entry));
    }
    Ok(entry)
}

/// Load `image` into `space`.
///
/// The space should be empty. Nothing here checks that: a new process's space
/// is fresh, and `execve` empties the running program's space first, after
/// [`check`] has said the image will not be refused for what it is.
///
/// # Errors
///
/// [`LoadError`]. On failure the space may hold part of the image.
pub(crate) fn load(space: &AddressSpace, image: &[u8]) -> Result<Loaded, LoadError> {
    let elf = Elf::parse(image).map_err(LoadError::Malformed)?;
    elf.check_machine(arch::ARCH.elf_machine())
        .map_err(|_| LoadError::WrongMachine(elf.machine()))?;
    elf.validate_segments().map_err(LoadError::Malformed)?;
    let (linked_low, linked_high) = elf.load_span(PAGE_SIZE).ok_or(LoadError::Empty)?;
    let bias = bias_of(&elf, linked_low)?;
    let entry = entry_point(&elf, bias)?;

    let low = linked_low.wrapping_add(bias);
    let high = linked_high.wrapping_add(bias);
    let span = high.checked_sub(low).ok_or(LoadError::Empty)?;

    // One writable region over the whole image, so that the copies below have
    // somewhere to land whatever the final permissions turn out to be.
    let _ = space.map_anonymous(low, span, VmaFlags::READ_WRITE)?;

    for segment in elf.loadable() {
        let data = segment.data(image).map_err(LoadError::Malformed)?;
        if !data.is_empty() {
            uaccess::copy_to_user(space, segment.vaddr.wrapping_add(bias), data)?;
        }
        // The rest of `p_memsz` is `.bss` and is already zero.
    }

    apply_permissions(space, &elf, bias, low, high)?;

    Ok(Loaded {
        entry,
        phdr: program_headers_at(&elf, bias),
        phent: u64::from(elf.header().phentsize),
        phnum: u64::from(elf.header().phnum),
        end: high,
    })
}

/// Give every page the permissions the segments covering it ask for.
fn apply_permissions(
    space: &AddressSpace,
    elf: &Elf<'_>,
    bias: u64,
    low: u64,
    high: u64,
) -> Result<(), LoadError> {
    let pages = usize::try_from((high - low) / PAGE_SIZE).map_err(|_| LoadError::Empty)?;
    let mut wanted: Vec<u32> = vec![0; pages];

    for segment in elf.loadable() {
        for index in pages_of(&segment, bias, low, pages) {
            if let Some(slot) = wanted.get_mut(index) {
                *slot |= segment.flags;
            }
        }
    }

    // Runs of pages that agree become one `protect` call each, which is also
    // what keeps the region map from growing a region per page.
    let mut start = 0_usize;
    while start < pages {
        let flags = *wanted.get(start).unwrap_or(&0);
        let mut end = start;
        while end < pages && wanted.get(end) == Some(&flags) {
            end += 1;
        }
        let at = low + (start as u64) * PAGE_SIZE;
        if flags & PF_W != 0 && flags & PF_X != 0 {
            return Err(LoadError::WriteExecute(at));
        }
        let len = ((end - start) as u64) * PAGE_SIZE;
        space.protect(at, len, permissions(flags))?;
        start = end;
    }
    Ok(())
}

/// The page indices, relative to `low`, that a segment occupies.
fn pages_of(segment: &Segment, bias: u64, low: u64, pages: usize) -> core::ops::Range<usize> {
    let Some(end) = segment.vaddr_end() else {
        return 0..0;
    };
    let first = segment.vaddr.wrapping_add(bias).saturating_sub(low) / PAGE_SIZE;
    let last = end
        .wrapping_add(bias)
        .saturating_sub(low)
        .div_ceil(PAGE_SIZE);
    let first = usize::try_from(first).unwrap_or(pages);
    let last = usize::try_from(last).unwrap_or(pages);
    first.min(pages)..last.min(pages)
}

/// `PF_*` as region flags.
fn permissions(flags: u32) -> VmaFlags {
    VmaFlags {
        read: flags & PF_R != 0,
        write: flags & PF_W != 0,
        execute: flags & PF_X != 0,
        ..VmaFlags::NONE
    }
}

/// Where the program header table ended up in memory.
///
/// musl reads it to find its own `PT_TLS` and `PT_GNU_RELRO`, so getting it
/// wrong is not cosmetic. The table is at a file offset; the address is that
/// offset translated through whichever loadable segment's file range contains
/// it, and moved as the image was. Zero if none does, which is what Linux
/// reports in the same case. A static PIE's start finds its own base from
/// this, so for one it has to be right to the byte.
fn program_headers_at(elf: &Elf<'_>, bias: u64) -> u64 {
    let phoff = elf.header().phoff;
    for segment in elf.loadable() {
        let Some(end) = segment.offset.checked_add(segment.filesz) else {
            continue;
        };
        if phoff >= segment.offset && phoff < end {
            return segment
                .vaddr
                .saturating_add(phoff - segment.offset)
                .wrapping_add(bias);
        }
    }
    0
}
