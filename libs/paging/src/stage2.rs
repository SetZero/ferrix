//! Arm stage-2 page table encoding: the tables an `SMMUv3` walks to translate
//! a device's DMA when its stream table entry selects stage 2, three levels
//! over a 4 KiB granule.
//!
//! Reference: Arm Architecture Reference Manual for A-profile, section D8,
//! stage 2 translation, and the `SMMUv3` specification's stream table entry;
//! checked against QEMU's `hw/arm/smmu-common.c`, which the boot test runs.
//!
//! The walk is the one an entry with `S2T0SZ = 25`, `S2SL0 = 1` and a 4 KiB
//! `S2TG` describes: 39 bits of input address, starting at level 1, which is
//! [`Level::GIGABYTE`]. It is the same format on both Arm architectures — an
//! `SMMUv3` walks `AArch64` tables whatever the processor is, and QEMU stops on
//! an entry that says otherwise — so ARMv7-A's kernel builds these too.
//!
//! A stage-2 descriptor is `AArch64`'s with its attributes re-cut: `S2AP` in
//! bits 7:6 grants read and write outright rather than by privilege level,
//! and `MemAttr` in bits 5:2 states the memory type itself instead of
//! indexing `MAIR`. User, global and execute have no meaning for a device, so
//! [`Encoding::leaf_flags`] reads them back as clear.

use crate::{Encoding, Level, MapFlags, PhysAddr, VirtAddr};

/// Descriptor is valid.
const VALID: u64 = 1 << 0;
/// At levels 1 and 2, distinguishes a table pointer from a block. At level 3
/// it is part of the page encoding and must be set.
const TABLE_OR_PAGE: u64 = 1 << 1;

/// `MemAttr`, bits 5:2.
const MEMATTR: u64 = 0b1111 << 2;
/// `MemAttr` for normal memory, cacheable for reads and writes.
const MEMATTR_NORMAL: u64 = 0b1111 << 2;
/// `MemAttr` for device-nGnRE memory, which a doorbell register is.
const MEMATTR_DEVICE: u64 = 0b0001 << 2;

/// `S2AP[0]`: the device may read.
const S2AP_READ: u64 = 1 << 6;
/// `S2AP[1]`: the device may write.
const S2AP_WRITE: u64 = 1 << 7;

/// Inner shareable, which normal memory on a multiprocessor has to be.
const SH_INNER: u64 = 0b11 << 8;

/// Access flag. An entry without it faults on first use unless the stream
/// table entry sets `S2AFFD`, which Ferrix does not.
const ACCESS_FLAG: u64 = 1 << 10;

/// Execute never. A device does not execute, so every leaf sets it.
const XN: u64 = 1 << 54;

/// Bits of a descriptor that hold an output address: 12..39, the 40-bit
/// output size an entry's `S2PS = 2` selects — all of ARMv7-A's physical
/// space, and all of QEMU `virt`'s RAM.
const ADDRESS_MASK: u64 = 0x0000_00FF_FFFF_F000;

/// Arm stage-2 translation, three levels over 39 bits.
#[derive(Clone, Copy, Debug)]
pub struct ArmStage2;

impl Encoding for ArmStage2 {
    const NAME: &'static str = "Arm stage 2";

    const ROOT_LEVEL: Level = Level::GIGABYTE;

    const VIRT_BITS: u32 = 39;

    const PHYS_BITS: u32 = 40;

    fn is_canonical(virt: VirtAddr) -> bool {
        virt.0 >> Self::VIRT_BITS == 0
    }

    fn canonical(address: u64) -> u64 {
        address
    }

    fn table_descriptor(table: PhysAddr, _flags: MapFlags) -> u64 {
        (table.0 & ADDRESS_MASK) | VALID | TABLE_OR_PAGE
    }

    fn leaf_descriptor(frame: PhysAddr, level: Level, flags: MapFlags) -> u64 {
        let mut entry = (frame.0 & ADDRESS_MASK) | VALID | ACCESS_FLAG | XN;
        if level == Level::PAGE {
            entry |= TABLE_OR_PAGE;
        }
        if flags.read {
            entry |= S2AP_READ;
        }
        if flags.write {
            entry |= S2AP_WRITE;
        }
        if flags.device {
            entry |= MEMATTR_DEVICE;
        } else {
            entry |= MEMATTR_NORMAL | SH_INNER;
        }
        entry
    }

    fn is_present(entry: u64) -> bool {
        entry & VALID != 0
    }

    fn is_leaf(entry: u64, level: Level) -> bool {
        level == Level::PAGE || entry & TABLE_OR_PAGE == 0
    }

    fn address(entry: u64) -> PhysAddr {
        PhysAddr(entry & ADDRESS_MASK)
    }

    fn supports_block(level: Level) -> bool {
        level == Level::GIGABYTE || level == Level::MEGABYTE
    }

    fn leaf_flags(entry: u64) -> MapFlags {
        MapFlags {
            read: entry & S2AP_READ != 0,
            write: entry & S2AP_WRITE != 0,
            execute: false,
            user: false,
            global: false,
            device: entry & MEMATTR == MEMATTR_DEVICE,
        }
    }
}
