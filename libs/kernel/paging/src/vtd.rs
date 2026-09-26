//! Intel VT-d second-level page table encoding: the tables a remapping unit
//! walks to translate a device's DMA, three levels over a 4 KiB granule.
//!
//! Reference: Intel Virtualization Technology for Directed I/O, Architecture
//! Specification, "Second-Level Paging Entries"; checked against QEMU's
//! `hw/i386/intel_iommu_internal.h`, which is what the boot test runs.
//!
//! A device's address is an I/O virtual address, not a processor's, and the
//! walk is the one its context entry's address width selects. Three levels
//! translate 39 bits — the one width QEMU's unit supports unless told
//! otherwise, `SAGAW` bit 1 — and a table built for it starts at
//! [`Level::GIGABYTE`], as ARMv7-A's does. Nothing is sign extended: an I/O
//! virtual address is a plain number below `1 << 39`.
//!
//! A descriptor says only what the device may do, read or write, so the
//! flags a processor mapping carries for its own sake — user, global,
//! execute, device memory — have no bit here, and
//! [`Encoding::leaf_flags`] reads them back as clear.
//!
//! A 2 MiB or 1 GiB leaf is legal only on a unit whose capability register
//! reports that page size, which the kernel checks before a domain's mapper
//! is allowed to build one.

use crate::{Encoding, Level, MapFlags, PhysAddr, VirtAddr};

/// The device may read through this entry.
const READ: u64 = 1 << 0;
/// The device may write through this entry.
const WRITE: u64 = 1 << 1;
/// At levels 1 and 2, the entry maps a 1 GiB or 2 MiB page rather than
/// pointing at a table.
const SUPERPAGE: u64 = 1 << 7;

/// Bits of a descriptor that hold a physical address: 12..38. The unit's
/// host address width bounds them, and QEMU's is 39 bits.
const ADDRESS_MASK: u64 = 0x0000_007F_FFFF_F000;

/// VT-d second-level translation, three levels over 39 bits.
#[derive(Clone, Copy, Debug)]
pub struct VtdSecondLevel;

impl Encoding for VtdSecondLevel {
    const NAME: &'static str = "VT-d second level";

    const ROOT_LEVEL: Level = Level::GIGABYTE;

    const VIRT_BITS: u32 = 39;

    const PHYS_BITS: u32 = 39;

    fn is_canonical(virt: VirtAddr) -> bool {
        virt.0 >> Self::VIRT_BITS == 0
    }

    fn canonical(address: u64) -> u64 {
        address
    }

    fn table_descriptor(table: PhysAddr, _flags: MapFlags) -> u64 {
        // What a device may do is the intersection of every entry on the
        // walk, so a table entry grants both and the leaf decides.
        (table.0 & ADDRESS_MASK) | READ | WRITE
    }

    fn leaf_descriptor(frame: PhysAddr, level: Level, flags: MapFlags) -> u64 {
        let mut entry = frame.0 & ADDRESS_MASK;
        if flags.read {
            entry |= READ;
        }
        if flags.write {
            entry |= WRITE;
        }
        if level != Level::PAGE {
            entry |= SUPERPAGE;
        }
        entry
    }

    fn is_present(entry: u64) -> bool {
        // There is no valid bit: an entry that permits nothing is not there.
        entry & (READ | WRITE) != 0
    }

    fn is_leaf(entry: u64, level: Level) -> bool {
        level == Level::PAGE || entry & SUPERPAGE != 0
    }

    fn address(entry: u64) -> PhysAddr {
        PhysAddr(entry & ADDRESS_MASK)
    }

    fn supports_block(level: Level) -> bool {
        level == Level::GIGABYTE || level == Level::MEGABYTE
    }

    fn leaf_flags(entry: u64) -> MapFlags {
        MapFlags {
            read: entry & READ != 0,
            write: entry & WRITE != 0,
            execute: false,
            user: false,
            global: false,
            device: false,
            uncached: false,
        }
    }
}
