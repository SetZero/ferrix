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

use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_elf::{Elf, ElfError, PF_R, PF_W, PF_X, Segment};
use ferrix_vma::VmaFlags;

use crate::arch;
use crate::syscall::uaccess::{self, UserError};
use crate::user::space::{AddressSpace, SpaceError};

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
    /// Position-independent, which needs relocations applied. `libs/elf` can
    /// read them — the UEFI loader relocates itself — but nothing here applies
    /// them yet, so a static-PIE binary is refused by name rather than
    /// jumped into at the wrong address.
    NeedsRelocation,
    /// It has no loadable segments at all.
    Empty,
    /// A page would have to be both writable and executable.
    WriteExecute(u64),
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

/// Load `image` into `space`.
///
/// The space should be empty. Nothing here checks that, because `execve` will
/// want to load into a *fresh* space and swap it in only once the load has
/// succeeded — replacing a running program's memory and then failing leaves
/// nothing to return to.
///
/// # Errors
///
/// [`LoadError`]. On failure the space may hold part of the image, which is
/// why the caller builds a new one rather than loading over the live one.
pub(crate) fn load(space: &AddressSpace, image: &[u8]) -> Result<Loaded, LoadError> {
    let elf = Elf::parse(image).map_err(LoadError::Malformed)?;
    elf.check_machine(arch::ARCH.elf_machine())
        .map_err(|_| LoadError::WrongMachine(elf.machine()))?;
    if elf.is_pie() {
        return Err(LoadError::NeedsRelocation);
    }
    elf.validate_segments().map_err(LoadError::Malformed)?;

    let (low, high) = elf.load_span(PAGE_SIZE).ok_or(LoadError::Empty)?;
    let span = high.checked_sub(low).ok_or(LoadError::Empty)?;

    // One writable region over the whole image, so that the copies below have
    // somewhere to land whatever the final permissions turn out to be.
    let _ = space.map_anonymous(low, span, VmaFlags::READ_WRITE)?;

    for segment in elf.loadable() {
        let data = segment.data(image).map_err(LoadError::Malformed)?;
        if !data.is_empty() {
            uaccess::copy_to_user(space, segment.vaddr, data)?;
        }
        // The rest of `p_memsz` is `.bss` and is already zero.
    }

    apply_permissions(space, &elf, low, high)?;

    Ok(Loaded {
        entry: elf.entry(),
        phdr: program_headers_at(&elf),
        phent: u64::from(elf.header().phentsize),
        phnum: u64::from(elf.header().phnum),
        end: high,
    })
}

/// Give every page the permissions the segments covering it ask for.
fn apply_permissions(
    space: &AddressSpace,
    elf: &Elf<'_>,
    low: u64,
    high: u64,
) -> Result<(), LoadError> {
    let pages = usize::try_from((high - low) / PAGE_SIZE).map_err(|_| LoadError::Empty)?;
    let mut wanted: Vec<u32> = vec![0; pages];

    for segment in elf.loadable() {
        for index in pages_of(&segment, low, pages) {
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
fn pages_of(segment: &Segment, low: u64, pages: usize) -> core::ops::Range<usize> {
    let Some(end) = segment.vaddr_end() else {
        return 0..0;
    };
    let first = segment.vaddr.saturating_sub(low) / PAGE_SIZE;
    let last = end.saturating_sub(low).div_ceil(PAGE_SIZE);
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
/// it. Zero if none does, which is what Linux reports in the same case.
fn program_headers_at(elf: &Elf<'_>) -> u64 {
    let phoff = elf.header().phoff;
    for segment in elf.loadable() {
        let Some(end) = segment.offset.checked_add(segment.filesz) else {
            continue;
        };
        if phoff >= segment.offset && phoff < end {
            return segment.vaddr.saturating_add(phoff - segment.offset);
        }
    }
    0
}
