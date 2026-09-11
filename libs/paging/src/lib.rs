//! Page table construction for x86-64 and AArch64.
//!
//! Both architectures use a four-level table of 512 eight-byte descriptors
//! over a 4 KiB granule, indexed by the same bits of the virtual address. Only
//! the *encoding* of a descriptor differs. So the walk is written once here,
//! generic over an [`Encoding`], and each architecture supplies roughly forty
//! lines of bit layout.
//!
//! This lives in `libs/` rather than in the kernel for the reason
//! `docs/ARCHITECTURE.md` gives: it is pure arithmetic over bytes, so
//! `cargo test`, Miri and a fuzzer can all reach it, and a mistake here is the
//! kind that writes to the wrong physical page and shows up somewhere else
//! entirely.
//!
//! # Physical and virtual addresses are different types
//!
//! [`PhysAddr`] and [`VirtAddr`] are newtypes with no arithmetic between them.
//! Confusing the two is this project's characteristic bug, and it is the one
//! the compiler can be made to catch for free.
//!
//! ```
//! # use ferrix_paging::{Mapper, MapFlags, PhysAddr, VirtAddr, x86_64::X86_64};
//! # fn example(memory: &mut impl ferrix_paging::PhysMem) -> Result<(), ferrix_paging::MapError> {
//! let root = memory.allocate_table().ok_or(ferrix_paging::MapError::OutOfMemory)?;
//! let mut mapper: Mapper<X86_64> = Mapper::new(root);
//! mapper.map_range(
//!     memory,
//!     VirtAddr(0xFFFF_FFFF_8000_0000),
//!     PhysAddr(0x20_0000),
//!     0x20_0000,
//!     MapFlags::KERNEL_CODE,
//! )?;
//! # Ok(())
//! # }
//! ```

#![no_std]

pub mod aarch64;
pub mod x86_64;

use core::fmt;
use core::marker::PhantomData;

/// Bytes in the smallest mapping either architecture is configured for.
pub const PAGE_SIZE: u64 = 4096;

/// Descriptors in one table. 4 KiB / 8 bytes.
pub const ENTRIES: usize = 512;

/// Bits of virtual address a four-level 4 KiB-granule table walk covers.
pub const VIRT_BITS: u32 = 48;

/// A physical address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PhysAddr(pub u64);

/// A virtual address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct VirtAddr(pub u64);

impl PhysAddr {
    /// True if the address is a multiple of `alignment`.
    #[must_use]
    pub const fn is_aligned_to(self, alignment: u64) -> bool {
        self.0 & (alignment - 1) == 0
    }

    /// This address advanced by `bytes`, or `None` on overflow.
    #[must_use]
    pub const fn checked_add(self, bytes: u64) -> Option<PhysAddr> {
        match self.0.checked_add(bytes) {
            Some(sum) => Some(PhysAddr(sum)),
            None => None,
        }
    }
}

impl VirtAddr {
    /// True if the address is a multiple of `alignment`.
    #[must_use]
    pub const fn is_aligned_to(self, alignment: u64) -> bool {
        self.0 & (alignment - 1) == 0
    }

    /// This address advanced by `bytes`, or `None` on overflow.
    #[must_use]
    pub const fn checked_add(self, bytes: u64) -> Option<VirtAddr> {
        match self.0.checked_add(bytes) {
            Some(sum) => Some(VirtAddr(sum)),
            None => None,
        }
    }

    /// True if bits 48..63 are a correct sign extension of bit 47.
    ///
    /// Both architectures fault on an address that is not, so a table built
    /// for one is a table with an entry nothing can ever reach.
    #[must_use]
    pub const fn is_canonical(self) -> bool {
        let high = self.0 >> (VIRT_BITS - 1);
        high == 0 || high == 0x1_FFFF
    }
}

impl fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhysAddr({:#018x})", self.0)
    }
}

impl fmt::Debug for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VirtAddr({:#018x})", self.0)
    }
}

/// Which level of the four-level walk a descriptor belongs to.
///
/// Level 0 is the root — the x86-64 PML4 and the AArch64 level-0 table — and
/// level 3 holds the 4 KiB leaves.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Level(u8);

impl Level {
    /// The root table.
    pub const ROOT: Level = Level(0);
    /// Where a 1 GiB block may be mapped.
    pub const GIGABYTE: Level = Level(1);
    /// Where a 2 MiB block may be mapped.
    pub const MEGABYTE: Level = Level(2);
    /// Where 4 KiB pages are mapped.
    pub const PAGE: Level = Level(3);

    /// Construct a level, `None` outside 0..=3.
    #[must_use]
    pub const fn new(level: u8) -> Option<Level> {
        if level <= 3 { Some(Level(level)) } else { None }
    }

    /// The level as a number, 0 at the root.
    #[must_use]
    pub const fn depth(self) -> u8 {
        self.0
    }

    /// How far to shift a virtual address to get this level's index.
    #[must_use]
    pub const fn shift(self) -> u32 {
        // Level 0 indexes bits 47..39, and each level down moves nine bits.
        39 - 9 * self.0 as u32
    }

    /// Bytes one descriptor at this level covers.
    #[must_use]
    pub const fn span(self) -> u64 {
        1 << self.shift()
    }

    /// This level's index within a virtual address.
    #[must_use]
    pub const fn index(self, virt: VirtAddr) -> usize {
        ((virt.0 >> self.shift()) & 0x1FF) as usize
    }

    /// The level below, or `None` at the leaves.
    #[must_use]
    pub const fn next(self) -> Option<Level> {
        Level::new(self.0 + 1)
    }
}

/// What a mapping may be used for.
///
/// Deliberately a struct of named booleans rather than a bitflags type: these
/// are written once per call site and read many times, and `MapFlags::KERNEL_
/// CODE` says more at a call site than an integer does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MapFlags {
    /// Readable. A mapping that is not readable is not mapped at all on either
    /// architecture, so this exists to make the intent explicit rather than to
    /// be switched off.
    pub read: bool,
    /// Writable.
    pub write: bool,
    /// Executable at the privilege level this mapping is for.
    pub execute: bool,
    /// Reachable from user mode.
    pub user: bool,
    /// Present in every address space, so the translation survives an address
    /// space switch. Kernel mappings only.
    pub global: bool,
    /// Device memory: uncached, and on AArch64 also non-gathering and
    /// non-reordering, which is what an `MMIO` register requires.
    pub device: bool,
}

impl MapFlags {
    /// Kernel text: read and execute, never writable.
    pub const KERNEL_CODE: MapFlags = MapFlags {
        read: true,
        write: false,
        execute: true,
        user: false,
        global: true,
        device: false,
    };

    /// Kernel constants: read only, never executable.
    pub const KERNEL_RODATA: MapFlags = MapFlags {
        read: true,
        write: false,
        execute: false,
        user: false,
        global: true,
        device: false,
    };

    /// Kernel data, stacks and the direct map: read and write, never
    /// executable. This is the one used for most of the address space, which
    /// is why W^X is its default rather than something switched on later.
    pub const KERNEL_DATA: MapFlags = MapFlags {
        read: true,
        write: true,
        execute: false,
        user: false,
        global: true,
        device: false,
    };

    /// A device register window.
    pub const KERNEL_DEVICE: MapFlags = MapFlags {
        read: true,
        write: true,
        execute: false,
        user: false,
        global: true,
        device: true,
    };

    /// User text.
    pub const USER_CODE: MapFlags = MapFlags {
        read: true,
        write: false,
        execute: true,
        user: true,
        global: false,
        device: false,
    };

    /// User data and stack.
    pub const USER_DATA: MapFlags = MapFlags {
        read: true,
        write: true,
        execute: false,
        user: true,
        global: false,
        device: false,
    };

    /// The same flags with execute permission removed.
    #[must_use]
    pub const fn without_execute(self) -> MapFlags {
        MapFlags {
            execute: false,
            ..self
        }
    }

    /// The same flags with write permission removed.
    #[must_use]
    pub const fn read_only(self) -> MapFlags {
        MapFlags {
            write: false,
            ..self
        }
    }
}

/// The bit layout of one architecture's page table descriptors.
pub trait Encoding {
    /// A short name, for diagnostics.
    const NAME: &'static str;

    /// Descriptor for an intermediate table at `table`.
    ///
    /// `flags` is passed because x86-64 needs the user bit set on every level
    /// of the walk, not only the leaf: an intermediate entry without it blocks
    /// user access to everything beneath.
    fn table_descriptor(table: PhysAddr, flags: MapFlags) -> u64;

    /// Descriptor for a leaf mapping `frame` at `level`.
    fn leaf_descriptor(frame: PhysAddr, level: Level, flags: MapFlags) -> u64;

    /// True if the descriptor maps or points at anything.
    fn is_present(entry: u64) -> bool;

    /// True if the descriptor is a leaf rather than a pointer to a table.
    fn is_leaf(entry: u64, level: Level) -> bool;

    /// The address a descriptor holds, whether leaf or table.
    fn address(entry: u64) -> PhysAddr;

    /// True if a block mapping is architecturally allowed at `level`.
    fn supports_block(level: Level) -> bool;
}

/// Access to physical memory, and a source of zeroed page table frames.
///
/// The mapper does no `unsafe` of its own: everything it needs to touch goes
/// through this trait, so the one place that has to be reasoned about is the
/// implementation. A test implements it over a map; the loader implements it
/// over the identity mapping firmware left in place; the kernel implements it
/// over the direct map.
///
/// # Safety
///
/// An implementation must guarantee that:
///
/// * `read` and `write` address real, mapped, naturally aligned physical
///   memory, and that no other reference aliases it;
/// * `allocate_table` returns a 4 KiB-aligned frame of `PAGE_SIZE` bytes,
///   filled with zeroes, that is not in use for anything else and stays valid
///   for as long as the table it becomes part of.
pub unsafe trait PhysMem {
    /// Read the eight bytes at `at`.
    fn read(&self, at: PhysAddr) -> u64;

    /// Write eight bytes at `at`.
    fn write(&mut self, at: PhysAddr, value: u64);

    /// Take a zeroed 4 KiB frame to use as a page table.
    fn allocate_table(&mut self) -> Option<PhysAddr>;
}

/// Why a mapping could not be established.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapError {
    /// The virtual address, physical address or length was not page aligned.
    Misaligned,
    /// The virtual address is not a canonical 48-bit address.
    NotCanonical,
    /// No frame was available for an intermediate table.
    OutOfMemory,
    /// Something is already mapped in the requested range. Carries the address.
    AlreadyMapped(VirtAddr),
    /// The walk met a block mapping where it needed a table, so the request
    /// would have had to split an existing large page. Carries the address.
    BlockInTheWay(VirtAddr),
    /// The range wraps the end of the address space.
    RangeOverflow,
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::Misaligned => f.write_str("address or length is not page aligned"),
            MapError::NotCanonical => f.write_str("virtual address is not canonical"),
            MapError::OutOfMemory => f.write_str("no frame available for a page table"),
            MapError::AlreadyMapped(at) => write!(f, "{at:?} is already mapped"),
            MapError::BlockInTheWay(at) => write!(f, "{at:?} is inside a larger block mapping"),
            MapError::RangeOverflow => f.write_str("range wraps the address space"),
        }
    }
}

/// Builds and inspects a page table tree.
#[derive(Debug)]
pub struct Mapper<E: Encoding> {
    root: PhysAddr,
    largest_block: Level,
    encoding: PhantomData<E>,
}

impl<E: Encoding> Mapper<E> {
    /// Wrap an existing root table.
    ///
    /// The default block size is 2 MiB, because 1 GiB pages are an optional
    /// x86-64 feature (`CPUID.80000001H:EDX.PDPE1GB`) and a table that uses
    /// one on a CPU without it faults on first touch. Call
    /// [`Mapper::allow_gigabyte_blocks`] once that has been checked.
    #[must_use]
    pub const fn new(root: PhysAddr) -> Self {
        Mapper {
            root,
            largest_block: Level::MEGABYTE,
            encoding: PhantomData,
        }
    }

    /// The physical address of the root table, for `CR3` or `TTBR`.
    #[must_use]
    pub const fn root(&self) -> PhysAddr {
        self.root
    }

    /// Permit 1 GiB block mappings, which makes the direct map of physical
    /// memory a few hundred descriptors instead of a few hundred thousand.
    pub const fn allow_gigabyte_blocks(&mut self) {
        self.largest_block = Level::GIGABYTE;
    }

    /// Map `len` bytes at `virt` onto `phys`.
    ///
    /// Uses the largest blocks alignment allows, so mapping the whole of
    /// physical memory is cheap. Fails rather than overwriting anything
    /// already mapped — a loader that silently replaces a mapping is a loader
    /// whose bugs appear after the jump.
    pub fn map_range(
        &self,
        memory: &mut impl PhysMem,
        virt: VirtAddr,
        phys: PhysAddr,
        len: u64,
        flags: MapFlags,
    ) -> Result<(), MapError> {
        if !virt.is_aligned_to(PAGE_SIZE) || !phys.is_aligned_to(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }
        if !len.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }
        if !virt.is_canonical() {
            return Err(MapError::NotCanonical);
        }

        let mut done = 0u64;
        while done < len {
            let at = virt.checked_add(done).ok_or(MapError::RangeOverflow)?;
            let frame = phys.checked_add(done).ok_or(MapError::RangeOverflow)?;
            let remaining = len - done;

            let level = self.choose_level(at, frame, remaining);
            self.map_one(memory, at, frame, level, flags)?;
            done += level.span();
        }

        Ok(())
    }

    /// The largest level whose block fits this address, frame and remainder.
    fn choose_level(&self, virt: VirtAddr, phys: PhysAddr, remaining: u64) -> Level {
        for depth in self.largest_block.depth()..Level::PAGE.depth() {
            let Some(level) = Level::new(depth) else {
                continue;
            };
            if !E::supports_block(level) {
                continue;
            }
            let span = level.span();
            if virt.is_aligned_to(span) && phys.is_aligned_to(span) && remaining >= span {
                return level;
            }
        }
        Level::PAGE
    }

    /// Install one leaf descriptor, creating the tables above it.
    fn map_one(
        &self,
        memory: &mut impl PhysMem,
        virt: VirtAddr,
        phys: PhysAddr,
        target: Level,
        flags: MapFlags,
    ) -> Result<(), MapError> {
        let mut table = self.root;
        let mut level = Level::ROOT;

        while level < target {
            let slot = descriptor_address(table, level.index(virt));
            let entry = memory.read(slot);

            table = if E::is_present(entry) {
                if E::is_leaf(entry, level) {
                    return Err(MapError::BlockInTheWay(virt));
                }
                E::address(entry)
            } else {
                let fresh = memory.allocate_table().ok_or(MapError::OutOfMemory)?;
                memory.write(slot, E::table_descriptor(fresh, flags));
                fresh
            };

            level = level.next().ok_or(MapError::BlockInTheWay(virt))?;
        }

        let slot = descriptor_address(table, target.index(virt));
        if E::is_present(memory.read(slot)) {
            return Err(MapError::AlreadyMapped(virt));
        }
        memory.write(slot, E::leaf_descriptor(phys, target, flags));
        Ok(())
    }

    /// Resolve `virt` the way the hardware would, or `None` if it is unmapped.
    pub fn translate(&self, memory: &impl PhysMem, virt: VirtAddr) -> Option<PhysAddr> {
        let mut table = self.root;
        let mut level = Level::ROOT;

        loop {
            let entry = memory.read(descriptor_address(table, level.index(virt)));
            if !E::is_present(entry) {
                return None;
            }
            if E::is_leaf(entry, level) {
                let offset = virt.0 & (level.span() - 1);
                return E::address(entry).checked_add(offset);
            }
            table = E::address(entry);
            level = level.next()?;
        }
    }

    /// The level at which `virt` is mapped, for tests and diagnostics.
    pub fn mapping_level(&self, memory: &impl PhysMem, virt: VirtAddr) -> Option<Level> {
        let mut table = self.root;
        let mut level = Level::ROOT;

        loop {
            let entry = memory.read(descriptor_address(table, level.index(virt)));
            if !E::is_present(entry) {
                return None;
            }
            if E::is_leaf(entry, level) {
                return Some(level);
            }
            table = E::address(entry);
            level = level.next()?;
        }
    }
}

/// Physical address of descriptor `index` in the table at `table`.
const fn descriptor_address(table: PhysAddr, index: usize) -> PhysAddr {
    PhysAddr(table.0 + (index as u64) * 8)
}

#[cfg(test)]
mod tests;
