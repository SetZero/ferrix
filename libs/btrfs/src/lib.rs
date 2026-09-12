//! The btrfs on-disk format, as pure parsing.
//!
//! This crate is the foundation of the btrfs read path: it turns bytes that
//! came off a block device into superblocks, chunk mappings, B-tree nodes and
//! item payloads. It does not read the device, does not cache, does not write
//! and knows nothing about transactions or allocation. Everything here is a
//! function of a byte slice.
//!
//! # Logical addresses are not physical addresses
//!
//! This is the one thing a caller has to internalise before any of the rest is
//! usable. Every address btrfs stores in a tree — a root pointer, a node's
//! `blockptr`, an extent's `disk_bytenr` — is a *logical* address in a single
//! flat address space that spans the whole filesystem. It is not an offset into
//! any device. The mapping from logical to physical lives in the chunk tree,
//! and the chunk tree can only be found by first bootstrapping a partial map
//! out of the superblock's system chunk array. So the boot order is fixed:
//!
//! 1. read the superblock at [`superblock::PRIMARY_OFFSET`],
//! 2. load its system chunk array into a [`chunk::ChunkMap`],
//! 3. use that map to read the chunk tree at `chunk_root`,
//! 4. feed every `CHUNK_ITEM` in the chunk tree into the same map,
//! 5. only now is `root` — the root tree — reachable.
//!
//! Nothing in this crate performs step 1, 3 or 5; those need a device. What it
//! provides is every parse and every check those steps depend on.
//!
//! # Totality
//!
//! This code parses a disk that may be corrupt or actively hostile, in ring 0,
//! where a panic is the machine. Every function here is total: for any input at
//! all it returns a value or a [`BtrfsError`]. That is enforced structurally
//! rather than by care — the crate is `#![forbid(unsafe_code)]`, every field is
//! pulled from a slice with [`slice::get`] and [`u64::from_le_bytes`], and
//! every length arithmetic is checked. Casting a slice to a `*const` and
//! reading the struct would be shorter, and would put the sharpest surface in
//! the system behind an unchecked pointer.
//!
//! # Endianness
//!
//! Every multi-byte field on a btrfs volume is little-endian, on every
//! architecture, including big-endian ones. The `from_le_bytes` calls in this
//! crate are therefore not a host assumption; they are the format.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

pub mod chunk;
pub mod compress;
pub mod crc32c;
pub mod items;
pub mod superblock;
pub mod tree;

pub use chunk::{ChunkItem, ChunkMap, ChunkMapEntry, ChunkProfile};
pub use crc32c::crc32c;
pub use superblock::Superblock;
pub use tree::{BtrfsKey, Item, KeyPtr, Node, NodeHeader};

/// Everything that can be wrong with the bytes handed to this crate.
///
/// The variants carry what was found rather than just naming the check,
/// because the first question about a filesystem that will not mount is always
/// "wrong by how much".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BtrfsError {
    /// The buffer is shorter than the structure being read out of it.
    Truncated {
        /// Bytes the structure needs.
        needed: usize,
        /// Bytes actually available.
        found: usize,
    },
    /// The superblock magic is not `_BHRfS_M`, so this is not btrfs at all.
    BadMagic,
    /// The stored checksum and the computed one disagree.
    BadChecksum {
        /// The checksum the structure claims for itself.
        stored: u32,
        /// The checksum its bytes actually have.
        computed: u32,
    },
    /// A checksum algorithm this crate does not implement. Carries the raw
    /// `csum_type`: 1 is xxhash64, 2 is sha256, 3 is blake2b.
    UnsupportedChecksum(u16),
    /// `sectorsize` is zero, not a power of two, or outside a sane range.
    BadSectorSize(u32),
    /// `nodesize` is zero, not a power of two, smaller than a node header or
    /// larger than btrfs permits.
    BadNodeSize(u32),
    /// A structure's self-recorded address does not match where it was read
    /// from, which means the read landed somewhere else entirely — a wrong
    /// chunk mapping, a stale pointer or a torn write.
    WrongAddress {
        /// The address the caller read from.
        expected: u64,
        /// The address the structure records for itself.
        found: u64,
    },
    /// A node claims more items than a node of its size could hold.
    TooManyItems {
        /// The claimed item count.
        nritems: u32,
        /// The largest count the node's size allows.
        capacity: u32,
    },
    /// A leaf item's data range falls outside the node, or overlaps the item
    /// array at the front of the node.
    ItemOutOfBounds {
        /// Index of the offending item within the leaf.
        slot: u32,
    },
    /// Keys within a node are not in ascending order, which would make every
    /// binary search over it return an arbitrary answer.
    ItemsOutOfOrder {
        /// Index of the first item that is not greater than its predecessor.
        slot: u32,
    },
    /// `sys_chunk_array_size` exceeds the 2048-byte array it describes.
    SysChunkArrayTooLarge(u32),
    /// A `CHUNK_ITEM` is internally inconsistent: no stripes, a zero length or
    /// stripe length, or fewer stripe records than `num_stripes` promises.
    BadChunk,
    /// A chunk uses a RAID profile whose logical-to-physical mapping this crate
    /// deliberately does not compute. Carries the chunk's raw type bits.
    ///
    /// Guessing at a striped or parity layout would return a plausible address
    /// that is simply the wrong one, so the read is refused instead.
    UnsupportedProfile(u64),
    /// The [`ChunkMap`]'s caller-supplied storage is full.
    ChunkMapFull,
    /// No chunk covers this logical address.
    NotMapped(u64),
    /// An item payload is malformed for its key type — a length field that
    /// cannot be right, or an extent type outside `0..=2`.
    BadItem {
        /// The key type byte the payload was parsed as.
        item_type: u8,
    },
    /// An extent names a compression algorithm outside `0..=3`.
    UnsupportedCompression(u8),
    /// A compressed stream is malformed: a bad header, a back-reference
    /// before the start of the output, a table that does not describe a code,
    /// or more output than the buffer holds. Carries the `compression` byte.
    BadCompressedData {
        /// The algorithm the stream was decoded as; see
        /// [`items::COMPRESS_ZSTD`] and friends.
        compression: u8,
    },
}

impl fmt::Display for BtrfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            BtrfsError::Truncated { needed, found } => {
                write!(f, "buffer holds {found} bytes but {needed} are needed")
            }
            BtrfsError::BadMagic => f.write_str("not a btrfs superblock"),
            BtrfsError::BadChecksum { stored, computed } => {
                write!(
                    f,
                    "checksum is {computed:#010x} but {stored:#010x} was stored"
                )
            }
            BtrfsError::UnsupportedChecksum(kind) => {
                write!(f, "checksum type {kind} is not implemented")
            }
            BtrfsError::BadSectorSize(size) => write!(f, "sectorsize {size} is not usable"),
            BtrfsError::BadNodeSize(size) => write!(f, "nodesize {size} is not usable"),
            BtrfsError::WrongAddress { expected, found } => {
                write!(
                    f,
                    "read {expected:#x} but the block says it lives at {found:#x}"
                )
            }
            BtrfsError::TooManyItems { nritems, capacity } => {
                write!(
                    f,
                    "node claims {nritems} items but can hold at most {capacity}"
                )
            }
            BtrfsError::ItemOutOfBounds { slot } => {
                write!(f, "item {slot} points outside its node")
            }
            BtrfsError::ItemsOutOfOrder { slot } => write!(f, "item {slot} breaks key order"),
            BtrfsError::SysChunkArrayTooLarge(size) => {
                write!(f, "system chunk array claims {size} bytes")
            }
            BtrfsError::BadChunk => f.write_str("chunk item is internally inconsistent"),
            BtrfsError::UnsupportedProfile(bits) => {
                write!(f, "chunk profile {bits:#x} is not supported for reading")
            }
            BtrfsError::ChunkMapFull => f.write_str("chunk map storage is full"),
            BtrfsError::NotMapped(logical) => {
                write!(f, "no chunk covers logical address {logical:#x}")
            }
            BtrfsError::BadItem { item_type } => {
                write!(f, "malformed payload for item type {item_type}")
            }
            BtrfsError::UnsupportedCompression(kind) => {
                write!(f, "compression type {kind} is not implemented")
            }
            BtrfsError::BadCompressedData { compression } => {
                write!(f, "corrupt stream for compression type {compression}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Little-endian field access
//
// Every field in the crate goes through one of these. They return `None`
// rather than an error so that call sites can chain with `?` inside an
// `Option`-returning helper and convert once, at the boundary.
// ---------------------------------------------------------------------------

/// Read a little-endian `u16` at `offset`, or `None` past the end.
pub(crate) fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(array_at::<2>(bytes, offset)?))
}

/// Read a little-endian `u32` at `offset`, or `None` past the end.
pub(crate) fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(array_at::<4>(bytes, offset)?))
}

/// Read a little-endian `u64` at `offset`, or `None` past the end.
pub(crate) fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(array_at::<8>(bytes, offset)?))
}

/// Read a single byte at `offset`, or `None` past the end.
pub(crate) fn u8_at(bytes: &[u8], offset: usize) -> Option<u8> {
    bytes.get(offset).copied()
}

/// Copy out a fixed-size byte array at `offset`, or `None` past the end.
pub(crate) fn array_at<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    bytes.get(offset..offset.checked_add(N)?)?.try_into().ok()
}

/// Borrow `len` bytes at `offset`, or `None` past the end.
pub(crate) fn slice_at(bytes: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
    bytes.get(offset..offset.checked_add(len)?)
}

/// The length a buffer must have, as a [`BtrfsError::Truncated`].
pub(crate) fn truncated(needed: usize, found: usize) -> BtrfsError {
    BtrfsError::Truncated { needed, found }
}

/// Verify a btrfs checksum field.
///
/// btrfs prefixes both the superblock and every tree node with a 32-byte
/// checksum field covering everything after it, so one helper serves both. For
/// `csum_type == 0` only the first four bytes of that field are meaningful;
/// the remaining 28 are zero padding and are deliberately not checked, because
/// a future checksum type is allowed to fill them and an old kernel refusing
/// the volume over padding would be the wrong failure.
pub(crate) fn verify_crc32c(bytes: &[u8]) -> Result<(), BtrfsError> {
    let stored = u32_at(bytes, 0).ok_or_else(|| truncated(CSUM_SIZE, bytes.len()))?;
    let body = bytes
        .get(CSUM_SIZE..)
        .ok_or_else(|| truncated(CSUM_SIZE, bytes.len()))?;
    let computed = crc32c(body);
    if stored == computed {
        Ok(())
    } else {
        Err(BtrfsError::BadChecksum { stored, computed })
    }
}

/// Size of the checksum field at the front of a superblock or node.
///
/// Fixed at 32 bytes whatever the algorithm, so that sha256 and blake2b fit
/// without moving any other field.
pub const CSUM_SIZE: usize = 32;

/// Whether `value` is a power of two and within `min..=max`.
///
/// btrfs block sizes are always powers of two, and code that divides or masks
/// with them — every offset-within-a-sector calculation in the read path —
/// silently produces nonsense if they are not.
pub(crate) fn is_valid_block_size(value: u32, min: u32, max: u32) -> bool {
    value >= min && value <= max && value.is_power_of_two()
}

#[cfg(test)]
mod tests;
