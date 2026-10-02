//! Which bytes of a function's configuration space its driver may write.
//!
//! A driver reads all of its function's 4 KiB and writes almost none of it
//! (`docs/NVIDIA.md` §12.1). The header, every capability the kernel
//! programs -- MSI, MSI-X, power management, PCI Express -- and the extended
//! capabilities that would let a device bypass translation or make functions
//! enumeration never saw are the kernel's. What is left to a driver is the
//! inside of the device's own vendor-specific capabilities: a standard one
//! (ID `0x09`) past its three-byte header, and an extended one (VSEC, ID
//! `0x000B`) past its eight-byte header. Nothing else, including bytes no
//! capability covers, until a driver names a register and a reason.
//!
//! One exception inside a vendor capability: virtio's
//! `VIRTIO_PCI_CAP_PCI_CFG` window. Its four `pci_cfg_data` bytes reach the
//! function's BARs, the pages of the MSI-X table the kernel withheld
//! included, so they are refused; its `bar`, `offset` and `length` fields
//! stay writable.
//!
//! The answer is an allowlist, [`Writable`], computed once from the two
//! capability lists. A list that loops, points outside its space, or holds a
//! vendor capability another capability starts inside of gives the function
//! no writable byte at all: a list with one bad link has no trustworthy
//! remainder.

use core::fmt;

use crate::capability::{Capabilities, ExtendedCapabilities, FIRST_STANDARD, ID_VENDOR};
use crate::{Address, CONFIG_SPACE_SIZE, ConfigSpace, LEGACY_CONFIG_SPACE_SIZE};

/// Bytes of a standard vendor capability's header: ID, next pointer, length.
pub const VENDOR_HEADER: u16 = 3;
/// Extended capability ID: vendor-specific (VSEC).
pub const EXTENDED_ID_VENDOR: u16 = 0x000B;
/// Bytes of a VSEC's header: the extended header and the vendor header.
pub const EXTENDED_VENDOR_HEADER: u16 = 8;
/// Bytes of an extended capability's header.
const EXTENDED_HEADER: u16 = 4;
/// Bytes of a standard capability's header: ID and next pointer.
const STANDARD_HEADER: u16 = 2;

/// Where virtio's `pci_cfg_data` lies in a `VIRTIO_PCI_CAP_PCI_CFG`
/// capability, from the capability: after the sixteen bytes of
/// `virtio_pci_cap`.
pub const VIRTIO_PCI_CFG_DATA: u16 = 16;
/// Bytes of `pci_cfg_data`.
pub const VIRTIO_PCI_CFG_DATA_LEN: u16 = 4;

/// How many capabilities a [`Writable`] remembers by offset, to name a
/// refused byte's register. A function with more is named less precisely,
/// never allowed more.
pub const NAMED_CAPABILITIES: usize = 32;

/// One capability, kept to name what a refused write would have reached.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Named {
    /// Where it starts.
    offset: u16,
    /// Its ID, standard or extended.
    id: u16,
    /// Whether it is in the extended list.
    extended: bool,
}

/// The bytes of one function's configuration space its driver may write,
/// one bit a byte, and the capabilities seen, to name the rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Writable {
    /// One bit per byte of [`CONFIG_SPACE_SIZE`].
    bits: [u64; 64],
    /// The capabilities walked, in list order, standard first.
    named: [Named; NAMED_CAPABILITIES],
    /// How many of `named` are filled.
    count: u8,
}

impl Writable {
    /// No byte writable, no capability known.
    pub const NONE: Writable = Writable {
        bits: [0; 64],
        named: [Named {
            offset: 0,
            id: 0,
            extended: false,
        }; NAMED_CAPABILITIES],
        count: 0,
    };

    /// Whether every byte of `width` bytes at `offset` is writable. A range
    /// that runs past the space is not.
    #[must_use]
    pub fn allows(&self, offset: u16, width: u16) -> bool {
        let Some(end) = offset.checked_add(width) else {
            return false;
        };
        width != 0 && end <= CONFIG_SPACE_SIZE && (offset..end).all(|at| self.allows_byte(at))
    }

    /// Whether the byte at `offset` is writable.
    #[must_use]
    pub fn allows_byte(&self, offset: u16) -> bool {
        let at = usize::from(offset);
        self.bits
            .get(at / 64)
            .is_some_and(|word| word & (1 << (at % 64)) != 0)
    }

    /// Whether no byte at all is writable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|word| *word == 0)
    }

    /// How many bytes are writable.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bits
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// What the byte at `offset` belongs to, for a refusal's line.
    #[must_use]
    pub const fn name(&self, offset: u16) -> Register<'_> {
        Register {
            writable: self,
            offset,
        }
    }

    /// Set or clear `start..end`, cut at the end of the space.
    fn set(&mut self, start: u16, end: u16, on: bool) {
        for at in start..end.min(CONFIG_SPACE_SIZE) {
            let at = usize::from(at);
            if let Some(word) = self.bits.get_mut(at / 64) {
                if on {
                    *word |= 1 << (at % 64);
                } else {
                    *word &= !(1 << (at % 64));
                }
            }
        }
    }

    /// Remember a capability for naming, if there is room.
    fn remember(&mut self, offset: u16, id: u16, extended: bool) {
        if let Some(slot) = self.named.get_mut(usize::from(self.count)) {
            *slot = Named {
                offset,
                id,
                extended,
            };
            self.count += 1;
        }
    }

    /// The remembered capabilities.
    fn capabilities(&self) -> &[Named] {
        self.named.get(..usize::from(self.count)).unwrap_or(&[])
    }
}

/// A byte of configuration space named for a line: a header field, or the
/// capability it is in or after.
#[derive(Clone, Copy, Debug)]
pub struct Register<'a> {
    /// The function's capabilities.
    writable: &'a Writable,
    /// The byte.
    offset: u16,
}

impl fmt::Display for Register<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let offset = self.offset;
        if offset < FIRST_STANDARD {
            return f.write_str(header_field(offset));
        }
        let extended = offset >= LEGACY_CONFIG_SPACE_SIZE;
        let nearest = self
            .writable
            .capabilities()
            .iter()
            .filter(|named| named.extended == extended && named.offset <= offset)
            .max_by_key(|named| named.offset);
        match nearest {
            Some(named) => write!(
                f,
                "in or after the {} at {:#x}",
                capability_name(named.id, named.extended),
                named.offset
            ),
            None => f.write_str("device-dependent space"),
        }
    }
}

/// A type 0 header's field at `offset`, below 0x40.
const fn header_field(offset: u16) -> &'static str {
    match offset {
        0x00..=0x01 => "vendor ID",
        0x02..=0x03 => "device ID",
        0x04..=0x05 => "COMMAND",
        0x06..=0x07 => "STATUS",
        0x08 => "revision",
        0x09..=0x0B => "class code",
        0x0C => "cache line size",
        0x0D => "latency timer",
        0x0E => "header type",
        0x0F => "BIST",
        0x10..=0x13 => "BAR 0",
        0x14..=0x17 => "BAR 1",
        0x18..=0x1B => "BAR 2",
        0x1C..=0x1F => "BAR 3",
        0x20..=0x23 => "BAR 4",
        0x24..=0x27 => "BAR 5",
        0x30..=0x33 => "expansion ROM",
        0x34 => "capabilities pointer",
        0x3C => "interrupt line",
        0x3D => "interrupt pin",
        _ => "header",
    }
}

/// A capability's name.
const fn capability_name(id: u16, extended: bool) -> &'static str {
    if extended {
        return match id {
            0x0001 => "AER extended capability",
            0x000B => "vendor extended capability",
            0x000D => "ACS extended capability",
            0x000F => "ATS extended capability",
            0x0010 => "SR-IOV extended capability",
            0x0013 => "PRI extended capability",
            0x0015 => "resizable BAR extended capability",
            0x001B => "PASID extended capability",
            0x001D => "DPC extended capability",
            _ => "extended capability",
        };
    }
    match id {
        0x01 => "power management capability",
        0x05 => "MSI capability",
        0x09 => "vendor capability",
        0x10 => "PCI Express capability",
        0x11 => "MSI-X capability",
        _ => "capability",
    }
}

/// How many capabilities, of both lists, [`writable`] records before it
/// refuses the function.
pub const STARTS: usize = 64 + 960 / 4;
/// How many vendor capabilities, of both lists, [`writable`] records before
/// it refuses the function.
pub const BODIES: usize = 64;

/// Why a function gets no writable byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Refused {
    /// A capability list loops or points outside its space, or holds more
    /// capabilities than the walk records.
    List,
    /// A capability starts inside a vendor capability's body.
    Overlap,
}

/// The bytes `function`'s driver may write, from its two capability lists.
/// `vendor` is the function's vendor ID, which says whether its vendor
/// capabilities are virtio's.
///
/// A list the walk refuses, or one in which a capability starts inside a
/// vendor capability's body, leaves nothing writable.
pub fn writable<C: ConfigSpace + ?Sized>(space: &C, function: Address, vendor: u16) -> Writable {
    let mut writable = Writable::NONE;
    if fill(&mut writable, space, function, vendor).is_err() {
        writable.bits = [0; 64];
    }
    writable
}

/// [`writable`]'s walk.
fn fill<C: ConfigSpace + ?Sized>(
    writable: &mut Writable,
    space: &C,
    function: Address,
    vendor: u16,
) -> Result<(), Refused> {
    // Each capability's start, by list, for the overlap test and to clear
    // headers afterwards; and each vendor body. A function with more than
    // these hold is refused whole rather than checked in part: a capability
    // left unrecorded would escape the overlap test and the header clearing.
    let mut starts = [(0_u16, false); STARTS];
    let mut started = 0;
    let mut bodies = [(0_u16, 0_u16, false); BODIES];
    let mut bodied = 0;

    for capability in Capabilities::new(space, function) {
        let capability = capability.map_err(|_| Refused::List)?;
        let at = capability.offset;
        writable.remember(at, u16::from(capability.id), false);
        *starts.get_mut(started).ok_or(Refused::List)? = (at, false);
        started += 1;
        if capability.id != ID_VENDOR {
            continue;
        }
        let len = u16::from(space.read8(function, at + 2));
        let end = at.saturating_add(len).min(LEGACY_CONFIG_SPACE_SIZE);
        if len <= VENDOR_HEADER {
            continue;
        }
        writable.set(at + VENDOR_HEADER, end, true);
        *bodies.get_mut(bodied).ok_or(Refused::List)? = (at, end, false);
        bodied += 1;
        if vendor == crate::virtio::VENDOR
            && space.read8(function, at + 3) == crate::virtio::CFG_PCI
        {
            let data = at + VIRTIO_PCI_CFG_DATA;
            writable.set(data, data + VIRTIO_PCI_CFG_DATA_LEN, false);
        }
    }
    for capability in ExtendedCapabilities::new(space, function) {
        let capability = capability.map_err(|_| Refused::List)?;
        let at = capability.offset;
        writable.remember(at, capability.id, true);
        *starts.get_mut(started).ok_or(Refused::List)? = (at, true);
        started += 1;
        if capability.id != EXTENDED_ID_VENDOR {
            continue;
        }
        // VSEC length: bits 31:20 of the vendor header's word.
        let len = (space.read32(function, at + EXTENDED_HEADER) >> 20) as u16;
        let end = at.saturating_add(len).min(CONFIG_SPACE_SIZE);
        if len <= EXTENDED_VENDOR_HEADER {
            continue;
        }
        writable.set(at + EXTENDED_VENDOR_HEADER, end, true);
        *bodies.get_mut(bodied).ok_or(Refused::List)? = (at, end, true);
        bodied += 1;
    }

    let starts = starts.get(..started).unwrap_or(&[]);
    let bodies = bodies.get(..bodied).unwrap_or(&[]);
    for &(owner, end, extended) in bodies {
        let inside = starts
            .iter()
            .any(|&(at, list)| list == extended && at != owner && owner < at && at < end);
        if inside {
            return Err(Refused::Overlap);
        }
    }
    // A capability's own header is never a driver's, whatever covers it.
    for &(at, extended) in starts {
        let header = if extended {
            EXTENDED_HEADER
        } else {
            STANDARD_HEADER
        };
        writable.set(at, at.saturating_add(header), false);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::format;
    use std::vec;
    use std::vec::Vec;

    use super::{Writable, writable};
    use crate::header::{STATUS, STATUS_CAPABILITIES_LIST};
    use crate::{Address, ConfigSpace};

    /// One function's 4 KiB, as bytes.
    struct Space(Vec<u8>);

    impl Space {
        fn new() -> Self {
            Space(vec![0; 4096])
        }
        fn put8(&mut self, at: u16, value: u8) {
            self.0[usize::from(at)] = value;
        }
        fn put16(&mut self, at: u16, value: u16) {
            let at = usize::from(at);
            self.0[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        fn put32(&mut self, at: u16, value: u32) {
            let at = usize::from(at);
            self.0[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        /// Point the standard list at `first`.
        fn list(&mut self, first: u8) {
            self.put16(STATUS, STATUS_CAPABILITIES_LIST);
            self.put8(0x34, first);
        }
        /// A standard capability.
        fn cap(&mut self, at: u16, id: u8, next: u8) {
            self.put8(at, id);
            self.put8(at + 1, next);
        }
        /// A standard vendor capability `len` bytes long.
        fn vendor(&mut self, at: u16, next: u8, len: u8) {
            self.cap(at, 0x09, next);
            self.put8(at + 2, len);
        }
        /// An extended capability.
        fn ext(&mut self, at: u16, id: u16, next: u16) {
            self.put32(at, u32::from(id) | 1 << 16 | u32::from(next) << 20);
        }
        /// A VSEC `len` bytes long.
        fn vsec(&mut self, at: u16, next: u16, len: u16) {
            self.ext(at, 0x000B, next);
            self.put32(at + 4, u32::from(len) << 20 | 0x1234);
        }
    }

    impl ConfigSpace for Space {
        fn read8(&self, _: Address, offset: u16) -> u8 {
            self.0.get(usize::from(offset)).copied().unwrap_or(0xFF)
        }
        fn read16(&self, f: Address, offset: u16) -> u16 {
            u16::from_le_bytes([self.read8(f, offset), self.read8(f, offset + 1)])
        }
        fn read32(&self, f: Address, offset: u16) -> u32 {
            u32::from(self.read16(f, offset)) | u32::from(self.read16(f, offset + 2)) << 16
        }
        fn write16(&mut self, _: Address, _: u16, _: u16) {}
        fn write32(&mut self, _: Address, _: u16, _: u32) {}
    }

    fn function() -> Address {
        Address::new(0, 1, 0, 0).unwrap()
    }

    /// Every byte of `start..end` writable or not, as `on` says.
    fn all(writable: &Writable, start: u16, end: u16, on: bool) -> bool {
        (start..end).all(|at| writable.allows_byte(at) == on)
    }

    /// A function shaped like the table's rows: MSI at 0x40, power
    /// management at 0x50, PCI Express at 0x60, MSI-X at 0x9C, a vendor
    /// capability of 0x14 bytes at 0xA8; extended ATS at 0x100, a VSEC of
    /// 0x20 bytes at 0x140, AER at 0x180.
    fn shaped() -> Space {
        let mut s = Space::new();
        s.list(0x40);
        s.cap(0x40, 0x05, 0x50);
        s.cap(0x50, 0x01, 0x60);
        s.cap(0x60, 0x10, 0x9C);
        s.cap(0x9C, 0x11, 0xA8);
        s.vendor(0xA8, 0, 0x14);
        s.ext(0x100, 0x000F, 0x140);
        s.vsec(0x140, 0x180, 0x20);
        s.ext(0x180, 0x0001, 0);
        s
    }

    /// The header, MSI, power management, PCI Express and MSI-X are the
    /// kernel's, whole; a vendor capability's body past its three-byte header
    /// is the driver's; device-dependent space no capability covers is not.
    ///
    /// Verifies: L.device.26
    #[test]
    fn only_vendor_capability_bodies_are_writable_in_legacy_space() {
        let w = writable(&shaped(), function(), 0x10DE);
        assert!(all(&w, 0x00, 0x40, false), "the header");
        assert!(all(&w, 0x40, 0x50, false), "MSI");
        assert!(all(&w, 0x50, 0x60, false), "power management");
        assert!(all(&w, 0x60, 0x9C, false), "PCI Express");
        assert!(all(&w, 0x9C, 0xA8, false), "MSI-X");
        assert!(all(&w, 0xA8, 0xAB, false), "the vendor header");
        assert!(all(&w, 0xAB, 0xBC, true), "the vendor body");
        assert!(all(&w, 0xBC, 0x100, false), "device-dependent space");
        assert!(w.allows(0xAC, 4) && !w.allows(0xB8, 8));
    }

    /// In extended space only a VSEC's body past its eight-byte header is
    /// writable; ATS, AER and the bytes after them are not.
    ///
    /// Verifies: L.device.26
    #[test]
    fn the_ats_capability_is_refused_and_a_vsec_body_is_writable() {
        let w = writable(&shaped(), function(), 0x10DE);
        assert!(all(&w, 0x100, 0x140, false), "ATS");
        assert!(all(&w, 0x140, 0x148, false), "the VSEC header");
        assert!(all(&w, 0x148, 0x160, true), "the VSEC body");
        assert!(all(&w, 0x160, 0x1000, false), "AER and the rest");
        assert_eq!(w.len(), 0x11 + 0x18);
    }

    /// virtio's `VIRTIO_PCI_CAP_PCI_CFG`: its `bar`, `offset` and `length`
    /// stay writable and its `pci_cfg_data` is refused; the same layout from
    /// another vendor keeps its data window writable.
    ///
    /// Verifies: L.device.26
    #[test]
    fn virtio_s_pci_cfg_data_is_refused_and_its_fields_are_not() {
        let mut s = Space::new();
        s.list(0x84);
        s.vendor(0x84, 0, 20);
        s.put8(0x87, 5);
        let w = writable(&s, function(), 0x1AF4);
        assert!(all(&w, 0x87, 0x94, true), "cfg_type, bar, offset, length");
        assert!(all(&w, 0x94, 0x98, false), "pci_cfg_data");
        assert!(!w.allows(0x94, 4) && w.allows(0x8C, 4) && w.allows(0x90, 4));
        let other = writable(&s, function(), 0x8086);
        assert!(all(&other, 0x94, 0x98, true), "another vendor's bytes");
    }

    /// A vendor capability whose length runs past the legacy space is cut
    /// at 0xFF; a VSEC's past 4 KiB at the end of the space.
    ///
    /// Verifies: L.device.26
    #[test]
    fn a_vendor_capability_running_past_its_space_is_cut() {
        let mut s = Space::new();
        s.list(0xF0);
        s.vendor(0xF0, 0, 0xFF);
        s.ext(0x100, 0x000B, 0);
        s.put32(0x104, 0xFFF << 20);
        let w = writable(&s, function(), 0x10DE);
        assert!(all(&w, 0xF3, 0x100, true));
        assert!(all(&w, 0x100, 0x108, false), "the next space's header");
        assert!(all(&w, 0x108, 0x1000, true), "cut at 4 KiB");
        assert!(!w.allows(0xFE, 4), "a range past the space");
    }

    /// A capability header inside another vendor capability's body is never
    /// writable, and a list where one starts inside a body refuses the whole
    /// function.
    ///
    /// Verifies: L.device.26
    #[test]
    fn overlapping_capabilities_leave_nothing_writable() {
        let mut s = Space::new();
        s.list(0x40);
        s.vendor(0x40, 0x48, 0x20);
        s.cap(0x48, 0x05, 0);
        let w = writable(&s, function(), 0x10DE);
        assert!(w.is_empty(), "MSI inside a vendor body");

        let mut s = Space::new();
        s.list(0x40);
        s.vendor(0x40, 0x60, 0x10);
        s.vendor(0x60, 0, 0x10);
        let w = writable(&s, function(), 0x10DE);
        assert!(all(&w, 0x43, 0x50, true) && all(&w, 0x63, 0x70, true));
        assert!(all(&w, 0x60, 0x63, false), "a header is never writable");
    }

    /// A looping list, standard or extended, leaves nothing writable.
    ///
    /// Verifies: L.device.26
    #[test]
    fn a_looping_list_leaves_nothing_writable() {
        let mut s = Space::new();
        s.list(0x40);
        s.vendor(0x40, 0x40, 0x10);
        assert!(writable(&s, function(), 0x10DE).is_empty(), "standard");

        let mut s = shaped();
        s.ext(0x180, 0x0001, 0x100);
        assert!(writable(&s, function(), 0x10DE).is_empty(), "extended");
    }

    /// More vendor capabilities than the walk records -- 65 VSECs -- leave
    /// nothing writable, rather than the first 64's bodies checked and the
    /// rest not.
    ///
    /// Verifies: L.device.26
    #[test]
    fn more_vendor_bodies_than_recorded_leave_nothing_writable() {
        let mut s = Space::new();
        let count = super::BODIES as u16 + 1;
        for i in 0..count {
            let at = 0x100 + i * 0x38;
            let next = if i + 1 == count { 0 } else { at + 0x38 };
            s.vsec(at, next, 0x10);
        }
        assert!(writable(&s, function(), 0x10DE).is_empty());

        let mut s = Space::new();
        for i in 0..super::BODIES as u16 {
            let at = 0x100 + i * 0x38;
            let next = if i + 1 == super::BODIES as u16 {
                0
            } else {
                at + 0x38
            };
            s.vsec(at, next, 0x10);
        }
        assert!(!writable(&s, function(), 0x10DE).is_empty(), "64 still fit");
    }

    /// An extended list longer than the walk records leaves nothing
    /// writable, a VSEC at its head included.
    ///
    /// Verifies: L.device.26
    #[test]
    fn an_extended_list_past_what_is_recorded_leaves_nothing_writable() {
        let mut s = Space::new();
        s.vsec(0x100, 0x110, 0x10);
        let count = super::STARTS as u16;
        for i in 0..count {
            let at = 0x110 + i * 8;
            let next = if i + 1 == count { 0 } else { at + 8 };
            s.ext(at, 0x0001, next);
        }
        assert!(writable(&s, function(), 0x10DE).is_empty());
    }

    /// A refused byte is named by its header field, or by the capability it
    /// is in or after.
    #[test]
    fn a_refused_byte_is_named() {
        let w = writable(&shaped(), function(), 0x10DE);
        assert_eq!(format!("{}", w.name(0x10)), "BAR 0");
        assert_eq!(format!("{}", w.name(0x04)), "COMMAND");
        assert_eq!(
            format!("{}", w.name(0x42)),
            "in or after the MSI capability at 0x40"
        );
        assert_eq!(
            format!("{}", w.name(0x106)),
            "in or after the ATS extended capability at 0x100"
        );
        let none = Writable::NONE;
        assert_eq!(format!("{}", none.name(0x80)), "device-dependent space");
    }
}
