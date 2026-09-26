//! Which disk a ring serves: where it is, what it calls itself, and the name
//! and numbers it gets.
//!
//! HELLO carries three facts about the disk besides its geometry. `location`
//! is its PCI address, the kernel's check that two drivers cannot claim one
//! device. `serial` is whatever the device answered to virtio-blk's `GET_ID`,
//! kept for a later `/dev/disk/by-id`. `name` is the node name devmgr chose.
//!
//! # Names and numbers
//!
//! devmgr owns the naming policy: it starts block drivers in PCI-address order
//! and names them `vda`, `vdb`, … in that order, so names are stable on a given
//! machine. A name is `vd` and one to three lowercase letters, NUL-padded to
//! eight bytes, which is Linux's virtio-blk scheme and runs out at `vdzzz`.
//!
//! The kernel, not the driver, chooses the device numbers, because a driver
//! that picked its own minor could collide with another's. The major is the
//! kernel's one virtio-blk major. The minor is derived from the name as Linux
//! derives it: the name's letters are a bijective base-26 numeral, so `vda` is
//! disk 0, `vdz` 25, `vdaa` 26 and `vdzzz` 18277, and the whole disk takes
//! minor `index × 16`, leaving the 15 minors after it for partitions. The
//! largest, `vdzzz`'s, is 292432, so a minor must be at least 19 bits wide; the
//! kernel's are 20, as Linux's are.
//!
//! Whether a name is *already published*, or a location *already served*, is
//! a question about every ring the kernel holds, so it belongs to the kernel
//! glue; this module only decides whether a name is well formed.
//! [`crate::Refusal::NameInUse`] and [`crate::Refusal::LocationInUse`] are the
//! reasons the glue sends.

use core::fmt;

/// Bytes of HELLO's `name`.
pub const NAME_BYTES: usize = 8;

/// Bytes of HELLO's `serial`: virtio-blk's `VIRTIO_BLK_ID_BYTES`.
pub const SERIAL_BYTES: usize = 20;

/// Minors each disk takes: the whole disk and 15 partitions.
pub const MINORS_PER_DISK: u32 = 16;

/// The index of `vdzzz`, the last name that fits: 26 + 26² + 26³ − 1.
pub const MAX_DISK_INDEX: u32 = 18277;

/// A PCI address as HELLO carries it: segment in bits 31:16, bus in 15:8,
/// device and function in 7:0.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Location(
    /// The word, as HELLO carries it.
    pub u32,
);

impl Location {
    /// The word HELLO and START carry.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// The location of `segment:bus:devfn`.
    #[must_use]
    pub const fn new(segment: u16, bus: u8, devfn: u8) -> Self {
        Location(((segment as u32) << 16) | ((bus as u32) << 8) | devfn as u32)
    }

    /// The PCI segment.
    #[must_use]
    pub const fn segment(self) -> u16 {
        (self.0 >> 16) as u16
    }

    /// The bus.
    #[must_use]
    pub const fn bus(self) -> u8 {
        (self.0 >> 8) as u8
    }

    /// Device and function together, as PCI's `devfn`.
    #[must_use]
    pub const fn devfn(self) -> u8 {
        self.0 as u8
    }

    /// The device number, 0 to 31.
    #[must_use]
    pub const fn device(self) -> u8 {
        self.devfn() >> 3
    }

    /// The function number, 0 to 7.
    #[must_use]
    pub const fn function(self) -> u8 {
        self.devfn() & 7
    }
}

impl fmt::Display for Location {
    /// `ssss:bb:dd.f`, as Linux prints a PCI address.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04x}:{:02x}:{:02x}.{}",
            self.segment(),
            self.bus(),
            self.device(),
            self.function()
        )
    }
}

/// The disk index a HELLO `name` denotes: `vda` is 0, `vdz` 25, `vdaa` 26,
/// `vdzzz` 18277.
///
/// `None` unless the name is `vd`, then one to three lowercase ASCII letters,
/// then only NUL bytes — which is also the check the kernel refuses a
/// malformed name by.
#[must_use]
pub fn disk_index(name: &[u8; NAME_BYTES]) -> Option<u32> {
    let [b'v', b'd', first, second, third, 0, 0, 0] = *name else {
        return None;
    };
    let letters = match (second, third) {
        (0, 0) => 1,
        (0, _) => return None,
        (_, 0) => 2,
        _ => 3,
    };
    let mut index = 0_u32;
    for letter in [first, second, third].into_iter().take(letters) {
        if !letter.is_ascii_lowercase() {
            return None;
        }
        // Each digit is 1 to 26, and three of them stay below 2^15.
        index = index * 26 + u32::from(letter - b'a') + 1;
    }
    // A name has at least one letter, so `index` is at least 1.
    Some(index - 1)
}

/// The minor of the whole disk `index` denotes: `index × 16`.
///
/// `None` past [`MAX_DISK_INDEX`], which no name can denote.
#[must_use]
pub const fn whole_disk_minor(index: u32) -> Option<u32> {
    if index > MAX_DISK_INDEX {
        None
    } else {
        Some(index * MINORS_PER_DISK)
    }
}

/// A well-formed disk name: `vd` and one to three lowercase letters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct DiskName([u8; NAME_BYTES]);

impl DiskName {
    /// Check HELLO's name bytes.
    #[must_use]
    pub fn new(bytes: [u8; NAME_BYTES]) -> Option<Self> {
        disk_index(&bytes).map(|_| DiskName(bytes))
    }

    /// The name of disk `index`, as Linux's `virtblk_name_format` writes it:
    /// `vda` for 0, `vdaa` for 26. `None` past [`MAX_DISK_INDEX`].
    #[must_use]
    pub fn for_index(index: u32) -> Option<Self> {
        if index > MAX_DISK_INDEX {
            return None;
        }
        // Least significant letter first; at most three, since the index is
        // in range.
        let mut letters = [0_u8; 3];
        let mut len = 0;
        let mut rest = index;
        for slot in &mut letters {
            // `rest % 26` is below 26.
            *slot = b'a' + (rest % 26) as u8;
            len += 1;
            match (rest / 26).checked_sub(1) {
                Some(next) => rest = next,
                None => break,
            }
        }
        let [low, middle, high] = letters;
        let bytes = match len {
            1 => [b'v', b'd', low, 0, 0, 0, 0, 0],
            2 => [b'v', b'd', middle, low, 0, 0, 0, 0],
            _ => [b'v', b'd', high, middle, low, 0, 0, 0],
        };
        Some(DiskName(bytes))
    }

    /// The bytes HELLO carries, NUL-padded.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; NAME_BYTES] {
        &self.0
    }

    /// The name without its padding, such as `"vda"`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let len = self
            .0
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(NAME_BYTES);
        self.0
            .get(..len)
            .and_then(|name| core::str::from_utf8(name).ok())
            .unwrap_or("")
    }

    /// The disk index: `vda` is 0.
    #[must_use]
    pub fn index(&self) -> u32 {
        // A `DiskName` was checked when it was made, so this always has a
        // value; zero is never reached.
        disk_index(&self.0).unwrap_or(0)
    }

    /// The whole disk's minor: [`DiskName::index`] × 16.
    #[must_use]
    pub fn minor(&self) -> u32 {
        self.index() * MINORS_PER_DISK
    }
}

impl fmt::Display for DiskName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The disk a ring serves, as an accepted HELLO describes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Identity {
    /// Its PCI address.
    pub location: Location,
    /// Its `GET_ID` bytes; all zero if the device did not answer.
    pub serial: [u8; SERIAL_BYTES],
    /// The node name devmgr chose.
    pub name: DiskName,
}
