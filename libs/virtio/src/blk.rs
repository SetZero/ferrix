//! virtio-blk's device protocol: its configuration space, its feature bits,
//! and the three-part request a block device reads off a split virtqueue.
//!
//! Virtio 1.2 §5.2 defines the block device in three pieces, and each is here
//! as data rather than as a driver: [`Config`] reads `struct
//! virtio_blk_config` field by field, the `FEATURE_*` constants and
//! [`DRIVER_FEATURES`] say which bits a read-mostly driver takes, and
//! [`plan`], [`publish`] and [`parse_completion`] turn a request into a
//! descriptor chain and a completion back into a status. What drives a device
//! — the order of the status protocol, what happens when it misbehaves, which
//! request a completion belongs to — is `ferrix-virtio-blk`'s, which runs in a
//! user process over kernel handles and so cannot live in the kernel's crates.
//!
//! # Device addresses are not physical addresses
//!
//! A ring-3 driver gets its DMA memory from `dma_pin`, which returns one
//! device address per page: what the device's DMA engine dereferences, through
//! an IOMMU domain, and which need not equal the page's physical address nor
//! follow the previous page's. So nothing here takes "the address of the
//! buffer". A [`Data`] names a byte range of a pinned region together with the
//! address array, and [`plan`] gives the device one descriptor per page,
//! joining two pages into one descriptor only where their device addresses
//! happen to be consecutive. Treating a two-page buffer as contiguous because
//! its virtual mapping is would hand the device an address that, under an
//! IOMMU, belongs to nobody — or to somebody.
//!
//! # A request is split, never truncated
//!
//! A device bounds a request twice: `seg_max` data descriptors per chain and
//! `size_max` bytes per descriptor. A request that does not fit is not the
//! device's problem and not an error either; [`plan`] returns the longest
//! prefix, in whole blocks, that does, and the caller publishes the rest as a
//! further chain with its own header. A prefix is always whole blocks because
//! QEMU refuses — with `VIRTIO_BLK_S_IOERR` — any read or write whose length is
//! not a multiple of the logical block size, and so should anything else.
//!
//! # Trust
//!
//! Configuration bytes and completions are the device's word. A field whose
//! feature was negotiated but which lies past the end of the configuration
//! block is an error, not a zero; a block size Linux would refuse is refused;
//! a completion claiming more bytes written than the chain had room for, or a
//! status byte virtio does not define, is an error the caller must treat as a
//! broken device.

use core::fmt;

use crate::pci::{FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1};
use crate::{Buffer, QueueError, QueueMemory, SplitQueue};

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Feature bits, virtio 1.2 §5.2.3, checked against Linux's
// `include/uapi/linux/virtio_blk.h`.
// ---------------------------------------------------------------------------

/// `VIRTIO_BLK_F_BARRIER`, legacy: the device supports request barriers.
/// Never accepted; a virtio 1.x driver has no use for it.
pub const FEATURE_BARRIER: u64 = 1 << 0;
/// `VIRTIO_BLK_F_SIZE_MAX`: [`Config::size_max`] bounds each segment.
pub const FEATURE_SIZE_MAX: u64 = 1 << 1;
/// `VIRTIO_BLK_F_SEG_MAX`: [`Config::seg_max`] bounds the data segments of
/// one request.
pub const FEATURE_SEG_MAX: u64 = 1 << 2;
/// `VIRTIO_BLK_F_GEOMETRY`: [`Config::geometry`] is valid.
pub const FEATURE_GEOMETRY: u64 = 1 << 4;
/// `VIRTIO_BLK_F_RO`: the disk is read-only.
pub const FEATURE_RO: u64 = 1 << 5;
/// `VIRTIO_BLK_F_BLK_SIZE`: [`Config::blk_size`] is the logical block size.
pub const FEATURE_BLK_SIZE: u64 = 1 << 6;
/// `VIRTIO_BLK_F_SCSI`, legacy: SCSI command passthrough. Never accepted.
pub const FEATURE_SCSI: u64 = 1 << 7;
/// `VIRTIO_BLK_F_FLUSH`: the device takes [`RequestType::Flush`].
pub const FEATURE_FLUSH: u64 = 1 << 9;
/// `VIRTIO_BLK_F_TOPOLOGY`: [`Config::topology`] is valid.
pub const FEATURE_TOPOLOGY: u64 = 1 << 10;
/// `VIRTIO_BLK_F_CONFIG_WCE`: the driver may switch the cache mode through
/// [`Config::writeback`].
pub const FEATURE_CONFIG_WCE: u64 = 1 << 11;
/// `VIRTIO_BLK_F_MQ`: the device has [`Config::num_queues`] request queues.
pub const FEATURE_MQ: u64 = 1 << 12;
/// `VIRTIO_BLK_F_DISCARD`: the device takes [`RequestType::Discard`].
pub const FEATURE_DISCARD: u64 = 1 << 13;
/// `VIRTIO_BLK_F_WRITE_ZEROES`: the device takes
/// [`RequestType::WriteZeroes`].
pub const FEATURE_WRITE_ZEROES: u64 = 1 << 14;
/// `VIRTIO_BLK_F_SECURE_ERASE`: the device takes
/// [`RequestType::SecureErase`].
pub const FEATURE_SECURE_ERASE: u64 = 1 << 16;
/// `VIRTIO_BLK_F_ZONED`: the device is a zoned block device.
pub const FEATURE_ZONED: u64 = 1 << 17;

/// The features a read-mostly driver accepts when the device offers them.
///
/// The rule is that a bit is accepted when it only tells the driver something
/// or only constrains what it sends, and declined when it would oblige the
/// driver to do something it does not:
///
/// * **Taken:** [`FEATURE_SIZE_MAX`] and [`FEATURE_SEG_MAX`], which are limits
///   [`plan`] honours; [`FEATURE_RO`], because a driver that declines it still
///   has a read-only disk; [`FEATURE_BLK_SIZE`], which [`Limits`] honours;
///   [`FEATURE_FLUSH`], without which a write is only as durable as the host's
///   cache; [`FEATURE_GEOMETRY`] and [`FEATURE_TOPOLOGY`], which are
///   information. And from the transport, [`FEATURE_VERSION_1`] — required —
///   and [`FEATURE_ACCESS_PLATFORM`], without which a device behind an IOMMU
///   refuses `FEATURES_OK` (virtio 1.2 §6.1), and with which it sends its DMA
///   through the domain the driver's addresses came from.
/// * **Declined:** [`FEATURE_CONFIG_WCE`], because this driver never changes
///   the cache mode; [`FEATURE_MQ`], because it runs one queue;
///   [`FEATURE_DISCARD`], [`FEATURE_WRITE_ZEROES`] and
///   [`FEATURE_SECURE_ERASE`], which only matter to a driver that sends those
///   requests; [`FEATURE_ZONED`], whose sequential-write rules this driver
///   does not keep — a host-managed zoned device then refuses `FEATURES_OK`,
///   which is the right outcome; the legacy [`FEATURE_BARRIER`] and
///   [`FEATURE_SCSI`]; and every transport ring feature — indirect
///   descriptors, `EVENT_IDX`, the packed ring — because [`SplitQueue`]
///   implements none of them.
pub const DRIVER_FEATURES: u64 = FEATURE_SIZE_MAX
    | FEATURE_SEG_MAX
    | FEATURE_GEOMETRY
    | FEATURE_RO
    | FEATURE_BLK_SIZE
    | FEATURE_FLUSH
    | FEATURE_TOPOLOGY
    | FEATURE_VERSION_1
    | FEATURE_ACCESS_PLATFORM;

/// The features without which the driver gives up: only
/// [`FEATURE_VERSION_1`]. A device without it speaks the legacy interface,
/// whose configuration layout and endianness are not these.
pub const REQUIRED_FEATURES: u64 = FEATURE_VERSION_1;

// ---------------------------------------------------------------------------
// Configuration space, virtio 1.2 §5.2.4.
// ---------------------------------------------------------------------------

/// Offset of `capacity`, in 512-byte sectors.
pub const CONFIG_CAPACITY: u32 = 0;
/// Offset of `size_max`.
pub const CONFIG_SIZE_MAX: u32 = 8;
/// Offset of `seg_max`.
pub const CONFIG_SEG_MAX: u32 = 12;
/// Offset of `geometry.cylinders`.
pub const CONFIG_GEOMETRY: u32 = 16;
/// Offset of `blk_size`.
pub const CONFIG_BLK_SIZE: u32 = 20;
/// Offset of `topology.physical_block_exp`.
pub const CONFIG_TOPOLOGY: u32 = 24;
/// Offset of `writeback`.
pub const CONFIG_WRITEBACK: u32 = 32;
/// Offset of `num_queues`.
pub const CONFIG_NUM_QUEUES: u32 = 34;
/// Offset of `max_discard_sectors`.
pub const CONFIG_DISCARD: u32 = 36;
/// Offset of `max_write_zeroes_sectors`.
pub const CONFIG_WRITE_ZEROES: u32 = 48;
/// Offset of `max_secure_erase_sectors`.
pub const CONFIG_SECURE_ERASE: u32 = 60;
/// Offset of `zoned.zone_sectors`.
pub const CONFIG_ZONED: u32 = 72;
/// Bytes of `struct virtio_blk_config` as virtio 1.2 defines it.
pub const CONFIG_LEN: u32 = 96;

/// A device's device-specific configuration block.
///
/// The widths are there because virtio 1.2 §4.1.3.1 has a driver access each
/// field of a PCI device's configuration with its natural width, and Linux
/// does. Offsets past [`DeviceConfig::config_len`] are never passed by
/// [`Config::read`].
pub trait DeviceConfig {
    /// Bytes in the block.
    fn config_len(&self) -> u32;
    /// The byte at `offset`.
    fn config_read8(&self, offset: u32) -> u8;
    /// The little-endian `u16` at `offset`.
    fn config_read16(&self, offset: u32) -> u16;
    /// The little-endian `u32` at `offset`.
    fn config_read32(&self, offset: u32) -> u32;
}

/// A configuration block that has already been copied out, as bytes.
impl DeviceConfig for [u8] {
    fn config_len(&self) -> u32 {
        u32::try_from(self.len()).unwrap_or(u32::MAX)
    }

    fn config_read8(&self, offset: u32) -> u8 {
        byte_at(self, offset)
    }

    fn config_read16(&self, offset: u32) -> u16 {
        u16::from_le_bytes([
            byte_at(self, offset),
            byte_at(self, offset.saturating_add(1)),
        ])
    }

    fn config_read32(&self, offset: u32) -> u32 {
        u32::from_le_bytes([
            byte_at(self, offset),
            byte_at(self, offset.saturating_add(1)),
            byte_at(self, offset.saturating_add(2)),
            byte_at(self, offset.saturating_add(3)),
        ])
    }
}

/// The byte at `offset`, or zero past the end.
fn byte_at(bytes: &[u8], offset: u32) -> u8 {
    usize::try_from(offset)
        .ok()
        .and_then(|at| bytes.get(at))
        .copied()
        .unwrap_or(0)
}

/// `struct virtio_blk_geometry`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    /// Cylinders.
    pub cylinders: u16,
    /// Heads.
    pub heads: u8,
    /// Sectors per track.
    pub sectors: u8,
}

/// The topology fields [`FEATURE_TOPOLOGY`] guards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Topology {
    /// Log2 of logical blocks per physical block.
    pub physical_block_exp: u8,
    /// Offset of the first aligned logical block.
    pub alignment_offset: u8,
    /// Suggested minimum I/O size, in logical blocks.
    pub min_io_size: u16,
    /// Optimal sustained I/O size, in logical blocks.
    pub opt_io_size: u32,
}

/// The limits of a range-based request: discard or secure erase.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RangeLimits {
    /// Largest range, in 512-byte sectors.
    pub max_sectors: u32,
    /// Most ranges in one request.
    pub max_segments: u32,
    /// Ranges must be aligned to this many sectors.
    pub sector_alignment: u32,
}

/// The fields [`FEATURE_WRITE_ZEROES`] guards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WriteZeroes {
    /// Largest range, in 512-byte sectors.
    pub max_sectors: u32,
    /// Most ranges in one request.
    pub max_segments: u32,
    /// Nonzero if a write-zeroes request may deallocate sectors.
    pub may_unmap: u8,
}

/// `struct virtio_blk_zoned_characteristics`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Zoned {
    /// Sectors per zone.
    pub zone_sectors: u32,
    /// Most zones open at once.
    pub max_open_zones: u32,
    /// Most zones active at once.
    pub max_active_zones: u32,
    /// Largest zone append, in sectors.
    pub max_append_sectors: u32,
    /// Write granularity, in bytes.
    pub write_granularity: u32,
    /// The zoned model: none, host-managed or host-aware.
    pub model: u8,
}

/// `struct virtio_blk_config`, each field present only if its feature was
/// negotiated.
///
/// `capacity` has no feature and is always there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// Size of the disk in 512-byte sectors, whatever the block size.
    pub capacity: u64,
    /// Largest data segment in bytes ([`FEATURE_SIZE_MAX`]).
    pub size_max: Option<u32>,
    /// Most data segments in one request ([`FEATURE_SEG_MAX`]).
    pub seg_max: Option<u32>,
    /// Legacy geometry ([`FEATURE_GEOMETRY`]).
    pub geometry: Option<Geometry>,
    /// Logical block size in bytes ([`FEATURE_BLK_SIZE`]).
    pub blk_size: Option<u32>,
    /// Topology ([`FEATURE_TOPOLOGY`]).
    pub topology: Option<Topology>,
    /// Cache mode, 0 writethrough and 1 writeback ([`FEATURE_CONFIG_WCE`]).
    pub writeback: Option<u8>,
    /// Request queues ([`FEATURE_MQ`]).
    pub num_queues: Option<u16>,
    /// Discard limits ([`FEATURE_DISCARD`]).
    pub discard: Option<RangeLimits>,
    /// Write-zeroes limits ([`FEATURE_WRITE_ZEROES`]).
    pub write_zeroes: Option<WriteZeroes>,
    /// Secure-erase limits ([`FEATURE_SECURE_ERASE`]).
    pub secure_erase: Option<RangeLimits>,
    /// Zoned characteristics ([`FEATURE_ZONED`]).
    pub zoned: Option<Zoned>,
}

/// Read a field guarded by `feature` and ending at `end`, if it was
/// negotiated.
fn guarded<S: DeviceConfig + ?Sized, T>(
    source: &S,
    features: u64,
    feature: u64,
    end: u32,
    read: impl FnOnce(&S) -> T,
) -> Result<Option<T>, BlkError> {
    if features & feature == 0 {
        return Ok(None);
    }
    if source.config_len() < end {
        return Err(BlkError::ConfigTruncated { feature });
    }
    Ok(Some(read(source)))
}

/// A range-limit triple at `at`.
fn range_limits<S: DeviceConfig + ?Sized>(source: &S, at: u32) -> RangeLimits {
    RangeLimits {
        max_sectors: source.config_read32(at),
        max_segments: source.config_read32(at + 4),
        sector_alignment: source.config_read32(at + 8),
    }
}

impl Config {
    /// Read the configuration a device with `features` negotiated presents.
    ///
    /// Reading the same block twice may give two answers — the device can
    /// change it — so a caller over a live transport compares
    /// `config_generation` before and after, and reads again if it moved.
    ///
    /// # Errors
    ///
    /// [`BlkError::ConfigTruncated`] if the block ends before `capacity`
    /// (reported with `feature` zero) or before a negotiated field.
    pub fn read<S: DeviceConfig + ?Sized>(source: &S, features: u64) -> Result<Self, BlkError> {
        if source.config_len() < CONFIG_SIZE_MAX {
            return Err(BlkError::ConfigTruncated { feature: 0 });
        }
        let capacity = u64::from(source.config_read32(CONFIG_CAPACITY))
            | u64::from(source.config_read32(CONFIG_CAPACITY + 4)) << 32;
        Ok(Config {
            capacity,
            size_max: guarded(source, features, FEATURE_SIZE_MAX, 12, |s| {
                s.config_read32(CONFIG_SIZE_MAX)
            })?,
            seg_max: guarded(source, features, FEATURE_SEG_MAX, 16, |s| {
                s.config_read32(CONFIG_SEG_MAX)
            })?,
            geometry: guarded(source, features, FEATURE_GEOMETRY, 20, |s| Geometry {
                cylinders: s.config_read16(CONFIG_GEOMETRY),
                heads: s.config_read8(CONFIG_GEOMETRY + 2),
                sectors: s.config_read8(CONFIG_GEOMETRY + 3),
            })?,
            blk_size: guarded(source, features, FEATURE_BLK_SIZE, 24, |s| {
                s.config_read32(CONFIG_BLK_SIZE)
            })?,
            topology: guarded(source, features, FEATURE_TOPOLOGY, 32, |s| Topology {
                physical_block_exp: s.config_read8(CONFIG_TOPOLOGY),
                alignment_offset: s.config_read8(CONFIG_TOPOLOGY + 1),
                min_io_size: s.config_read16(CONFIG_TOPOLOGY + 2),
                opt_io_size: s.config_read32(CONFIG_TOPOLOGY + 4),
            })?,
            writeback: guarded(source, features, FEATURE_CONFIG_WCE, 33, |s| {
                s.config_read8(CONFIG_WRITEBACK)
            })?,
            num_queues: guarded(source, features, FEATURE_MQ, 36, |s| {
                s.config_read16(CONFIG_NUM_QUEUES)
            })?,
            discard: guarded(source, features, FEATURE_DISCARD, 48, |s| {
                range_limits(s, CONFIG_DISCARD)
            })?,
            write_zeroes: guarded(source, features, FEATURE_WRITE_ZEROES, 57, |s| {
                WriteZeroes {
                    max_sectors: s.config_read32(CONFIG_WRITE_ZEROES),
                    max_segments: s.config_read32(CONFIG_WRITE_ZEROES + 4),
                    may_unmap: s.config_read8(CONFIG_WRITE_ZEROES + 8),
                }
            })?,
            secure_erase: guarded(source, features, FEATURE_SECURE_ERASE, 72, |s| {
                range_limits(s, CONFIG_SECURE_ERASE)
            })?,
            zoned: guarded(source, features, FEATURE_ZONED, 93, |s| Zoned {
                zone_sectors: s.config_read32(CONFIG_ZONED),
                max_open_zones: s.config_read32(CONFIG_ZONED + 4),
                max_active_zones: s.config_read32(CONFIG_ZONED + 8),
                max_append_sectors: s.config_read32(CONFIG_ZONED + 12),
                write_granularity: s.config_read32(CONFIG_ZONED + 16),
                model: s.config_read8(CONFIG_ZONED + 20),
            })?,
        })
    }

    /// Write the fields this configuration has into `out`, as a device
    /// presents them. Bytes past the end of `out` are dropped; fields the
    /// configuration lacks are left as they were.
    ///
    /// For the device side: the kernel serving a virtqueue, and test devices.
    pub fn encode(&self, out: &mut [u8]) {
        put(out, CONFIG_CAPACITY, &self.capacity.to_le_bytes());
        if let Some(size_max) = self.size_max {
            put(out, CONFIG_SIZE_MAX, &size_max.to_le_bytes());
        }
        if let Some(seg_max) = self.seg_max {
            put(out, CONFIG_SEG_MAX, &seg_max.to_le_bytes());
        }
        if let Some(geometry) = self.geometry {
            put(out, CONFIG_GEOMETRY, &geometry.cylinders.to_le_bytes());
            put(
                out,
                CONFIG_GEOMETRY + 2,
                &[geometry.heads, geometry.sectors],
            );
        }
        if let Some(blk_size) = self.blk_size {
            put(out, CONFIG_BLK_SIZE, &blk_size.to_le_bytes());
        }
        if let Some(topology) = self.topology {
            put(
                out,
                CONFIG_TOPOLOGY,
                &[topology.physical_block_exp, topology.alignment_offset],
            );
            put(
                out,
                CONFIG_TOPOLOGY + 2,
                &topology.min_io_size.to_le_bytes(),
            );
            put(
                out,
                CONFIG_TOPOLOGY + 4,
                &topology.opt_io_size.to_le_bytes(),
            );
        }
        if let Some(writeback) = self.writeback {
            put(out, CONFIG_WRITEBACK, &[writeback]);
        }
        if let Some(num_queues) = self.num_queues {
            put(out, CONFIG_NUM_QUEUES, &num_queues.to_le_bytes());
        }
        self.encode_ranges(out);
    }

    /// The second half of [`Config::encode`]: the range and zoned fields.
    fn encode_ranges(&self, out: &mut [u8]) {
        let triple = |out: &mut [u8], at: u32, limits: RangeLimits| {
            put(out, at, &limits.max_sectors.to_le_bytes());
            put(out, at + 4, &limits.max_segments.to_le_bytes());
            put(out, at + 8, &limits.sector_alignment.to_le_bytes());
        };
        if let Some(discard) = self.discard {
            triple(out, CONFIG_DISCARD, discard);
        }
        if let Some(zeroes) = self.write_zeroes {
            put(out, CONFIG_WRITE_ZEROES, &zeroes.max_sectors.to_le_bytes());
            put(
                out,
                CONFIG_WRITE_ZEROES + 4,
                &zeroes.max_segments.to_le_bytes(),
            );
            put(out, CONFIG_WRITE_ZEROES + 8, &[zeroes.may_unmap]);
        }
        if let Some(erase) = self.secure_erase {
            triple(out, CONFIG_SECURE_ERASE, erase);
        }
        if let Some(zoned) = self.zoned {
            put(out, CONFIG_ZONED, &zoned.zone_sectors.to_le_bytes());
            put(out, CONFIG_ZONED + 4, &zoned.max_open_zones.to_le_bytes());
            put(out, CONFIG_ZONED + 8, &zoned.max_active_zones.to_le_bytes());
            put(
                out,
                CONFIG_ZONED + 12,
                &zoned.max_append_sectors.to_le_bytes(),
            );
            put(
                out,
                CONFIG_ZONED + 16,
                &zoned.write_granularity.to_le_bytes(),
            );
            put(out, CONFIG_ZONED + 20, &[zoned.model]);
        }
    }
}

/// Copy `bytes` into `out` at `at`, dropping whatever does not fit.
fn put(out: &mut [u8], at: u32, bytes: &[u8]) {
    let Ok(at) = usize::try_from(at) else {
        return;
    };
    for (index, value) in bytes.iter().enumerate() {
        if let Some(slot) = at.checked_add(index).and_then(|i| out.get_mut(i)) {
            *slot = *value;
        }
    }
}

// ---------------------------------------------------------------------------
// Limits: what a request may look like on this device.
// ---------------------------------------------------------------------------

/// The unit every sector number and `capacity` is counted in, whatever the
/// block size (virtio 1.2 §5.2.6).
pub const SECTOR_SIZE: u32 = 512;

/// The largest logical block size accepted.
///
/// Linux refuses a virtio disk whose `blk_size` is below 512, above a page,
/// or not a power of two (`blk_validate_block_size`), and a device reporting
/// one is broken rather than exotic: a data length that is a multiple of it
/// could not be split at page boundaries.
pub const MAX_BLOCK_SIZE: u32 = 4096;

/// The size of the pages `dma_pin` returns one device address for.
pub const PAGE_SIZE: u64 = 4096;

/// The most data descriptors [`plan`] puts in one chain, whatever `seg_max`
/// allows.
///
/// The chain is assembled on the stack before [`SplitQueue::add_chain`]
/// copies it into the table, so this is a stack bound — two header-and-status
/// descriptors more, 128 of 16 bytes. It costs nothing in reach: at one page
/// a descriptor it is half a megabyte a chain, and a larger request is split.
pub const MAX_DATA_SEGMENTS: usize = 126;

/// The most descriptors one request chain has: [`MAX_DATA_SEGMENTS`], the
/// header and the status byte.
pub const MAX_CHAIN: usize = MAX_DATA_SEGMENTS + 2;

/// What a device's configuration allows a request to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// The disk's size in 512-byte sectors.
    pub capacity: u64,
    /// The logical block size: every data length is a multiple of it, and
    /// every sector a multiple of it over 512.
    pub block_size: u32,
    /// Most data descriptors in one chain, at least one.
    pub max_segments: u32,
    /// Most bytes in one data descriptor, at least one.
    pub max_segment_size: u32,
}

impl Limits {
    /// The limits `config` sets.
    ///
    /// Without `blk_size` the block size is 512. Without `seg_max`, or with
    /// zero, a chain has one data segment: that is what Linux assumes of a
    /// device that states no limit, and the only safe reading of one that
    /// states zero. Without `size_max`, or with zero, a segment is unbounded.
    ///
    /// # Errors
    ///
    /// [`BlkError::BadBlockSize`] for a `blk_size` that is not a power of two
    /// from [`SECTOR_SIZE`] to [`MAX_BLOCK_SIZE`].
    pub fn new(config: &Config) -> Result<Self, BlkError> {
        let block_size = match config.blk_size {
            None => SECTOR_SIZE,
            Some(size)
                if size.is_power_of_two() && (SECTOR_SIZE..=MAX_BLOCK_SIZE).contains(&size) =>
            {
                size
            }
            Some(size) => return Err(BlkError::BadBlockSize(size)),
        };
        let most = MAX_DATA_SEGMENTS as u32;
        Ok(Limits {
            capacity: config.capacity,
            block_size,
            max_segments: config.seg_max.map_or(1, |seg_max| seg_max.clamp(1, most)),
            max_segment_size: match config.size_max {
                None | Some(0) => u32::MAX,
                Some(size_max) => size_max,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Requests, virtio 1.2 §5.2.6.
// ---------------------------------------------------------------------------

/// `VIRTIO_BLK_T_IN`: read.
pub const TYPE_IN: u32 = 0;
/// `VIRTIO_BLK_T_OUT`: write.
pub const TYPE_OUT: u32 = 1;
/// `VIRTIO_BLK_T_FLUSH`: flush the device's volatile cache.
pub const TYPE_FLUSH: u32 = 4;
/// `VIRTIO_BLK_T_GET_ID`: read the device's identifier string.
pub const TYPE_GET_ID: u32 = 8;
/// `VIRTIO_BLK_T_DISCARD`.
pub const TYPE_DISCARD: u32 = 11;
/// `VIRTIO_BLK_T_WRITE_ZEROES`.
pub const TYPE_WRITE_ZEROES: u32 = 13;
/// `VIRTIO_BLK_T_SECURE_ERASE`.
pub const TYPE_SECURE_ERASE: u32 = 14;

/// `VIRTIO_BLK_S_OK`.
pub const STATUS_OK: u8 = 0;
/// `VIRTIO_BLK_S_IOERR`.
pub const STATUS_IOERR: u8 = 1;
/// `VIRTIO_BLK_S_UNSUPP`.
pub const STATUS_UNSUPP: u8 = 2;

/// Bytes of a request header: `type`, `reserved`, `sector`.
pub const HEADER_LEN: u32 = 16;
/// Bytes of the status a device writes at the end of a chain.
pub const STATUS_LEN: u32 = 1;
/// Bytes of the identifier [`RequestType::GetId`] reads.
pub const ID_BYTES: u32 = 20;
/// Bytes of one `struct virtio_blk_discard_write_zeroes` range.
pub const RANGE_LEN: u32 = 16;
/// `VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP`, in a range's `flags`.
pub const WRITE_ZEROES_FLAG_UNMAP: u32 = 1;

/// A request's `type`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RequestType {
    /// Read whole blocks.
    In,
    /// Write whole blocks.
    Out,
    /// Flush the volatile cache. No data.
    Flush,
    /// Read the [`ID_BYTES`]-byte identifier.
    GetId,
    /// Discard the ranges the data holds.
    Discard,
    /// Zero the ranges the data holds.
    WriteZeroes,
    /// Securely erase the ranges the data holds.
    SecureErase,
}

impl RequestType {
    /// The `type` word.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            RequestType::In => TYPE_IN,
            RequestType::Out => TYPE_OUT,
            RequestType::Flush => TYPE_FLUSH,
            RequestType::GetId => TYPE_GET_ID,
            RequestType::Discard => TYPE_DISCARD,
            RequestType::WriteZeroes => TYPE_WRITE_ZEROES,
            RequestType::SecureErase => TYPE_SECURE_ERASE,
        }
    }

    /// The request a `type` word names, if it is one of these.
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            TYPE_IN => RequestType::In,
            TYPE_OUT => RequestType::Out,
            TYPE_FLUSH => RequestType::Flush,
            TYPE_GET_ID => RequestType::GetId,
            TYPE_DISCARD => RequestType::Discard,
            TYPE_WRITE_ZEROES => RequestType::WriteZeroes,
            TYPE_SECURE_ERASE => RequestType::SecureErase,
            _ => return None,
        })
    }

    /// Whether the device writes this request's data, rather than reading it.
    /// Meaningless for [`RequestType::Flush`], which has none.
    #[must_use]
    pub const fn device_writes_data(self) -> bool {
        matches!(self, RequestType::In | RequestType::GetId)
    }
}

/// A request header, as the device reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// What is asked.
    pub kind: RequestType,
    /// The first 512-byte sector. Zero for anything but a read or a write.
    pub sector: u64,
}

impl Header {
    /// The sixteen bytes of the header: `type`, a zero `reserved`, `sector`,
    /// little-endian.
    #[must_use]
    pub const fn encode(&self) -> [u8; HEADER_LEN as usize] {
        let kind = self.kind.code().to_le_bytes();
        let sector = self.sector.to_le_bytes();
        [
            kind[0], kind[1], kind[2], kind[3], 0, 0, 0, 0, sector[0], sector[1], sector[2],
            sector[3], sector[4], sector[5], sector[6], sector[7],
        ]
    }

    /// Read a header back, as the device does. `reserved` is ignored, as the
    /// specification tells a device to.
    ///
    /// # Errors
    ///
    /// [`BlkError::UnknownRequestType`] for a `type` this module does not
    /// define; a device answers such a request `VIRTIO_BLK_S_UNSUPP`.
    pub const fn decode(bytes: [u8; HEADER_LEN as usize]) -> Result<Self, BlkError> {
        let [t0, t1, t2, t3, _, _, _, _, s0, s1, s2, s3, s4, s5, s6, s7] = bytes;
        let code = u32::from_le_bytes([t0, t1, t2, t3]);
        let Some(kind) = RequestType::from_code(code) else {
            return Err(BlkError::UnknownRequestType(code));
        };
        Ok(Header {
            kind,
            sector: u64::from_le_bytes([s0, s1, s2, s3, s4, s5, s6, s7]),
        })
    }
}

/// How a device says a request went.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// `VIRTIO_BLK_S_OK`.
    Ok,
    /// `VIRTIO_BLK_S_IOERR`.
    IoError,
    /// `VIRTIO_BLK_S_UNSUPP`.
    Unsupported,
}

impl Status {
    /// The status byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Status::Ok => STATUS_OK,
            Status::IoError => STATUS_IOERR,
            Status::Unsupported => STATUS_UNSUPP,
        }
    }

    /// The status a byte names.
    ///
    /// The zoned statuses, 3 to 6, are errors like any other undefined value:
    /// this driver declines [`FEATURE_ZONED`], so a device has no business
    /// sending them.
    ///
    /// # Errors
    ///
    /// [`BlkError::BadStatus`] for any other byte.
    pub const fn from_byte(byte: u8) -> Result<Self, BlkError> {
        match byte {
            STATUS_OK => Ok(Status::Ok),
            STATUS_IOERR => Ok(Status::IoError),
            STATUS_UNSUPP => Ok(Status::Unsupported),
            _ => Err(BlkError::BadStatus(byte)),
        }
    }
}

/// Why a block request could not be built, or a completion or configuration
/// cannot be believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlkError {
    /// The configuration block ends before a field whose feature was
    /// negotiated; `feature` zero means before `capacity`.
    ConfigTruncated {
        /// The feature whose field is missing.
        feature: u64,
    },
    /// `blk_size` is not a power of two from 512 to [`MAX_BLOCK_SIZE`].
    BadBlockSize(u32),
    /// A header's `type` is not one virtio defines for a block device.
    UnknownRequestType(u32),
    /// The status byte is not one virtio defines.
    BadStatus(u8),
    /// The device says it wrote more than the chain had room for.
    WrittenTooLong {
        /// What the device claimed.
        written: u32,
        /// What the chain's writable descriptors hold.
        writable: u32,
    },
    /// The data reaches past the pages the region has.
    OutsideRegion,
    /// A device address plus a length does not fit in 64 bits.
    AddressOverflow,
    /// The data's length is not a whole number of units.
    NotWholeUnits,
    /// Not even one unit of data fits in the descriptors allowed.
    Unsplittable,
    /// The queue refused the chain.
    Queue(QueueError),
}

impl From<QueueError> for BlkError {
    fn from(error: QueueError) -> Self {
        BlkError::Queue(error)
    }
}

impl fmt::Display for BlkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            BlkError::ConfigTruncated { feature } => {
                write!(
                    f,
                    "the configuration ends before feature {feature:#x}'s field"
                )
            }
            BlkError::BadBlockSize(size) => write!(f, "block size {size} is not usable"),
            BlkError::UnknownRequestType(code) => write!(f, "request type {code} is unknown"),
            BlkError::BadStatus(byte) => write!(f, "status {byte:#x} is not a virtio status"),
            BlkError::WrittenTooLong { written, writable } => write!(
                f,
                "the device claims {written} bytes written into a chain holding {writable}"
            ),
            BlkError::OutsideRegion => f.write_str("the data is outside its pinned region"),
            BlkError::AddressOverflow => f.write_str("a device address overflows"),
            BlkError::NotWholeUnits => f.write_str("the data is not whole blocks"),
            BlkError::Unsplittable => f.write_str("not one block fits the segments allowed"),
            BlkError::Queue(error) => write!(f, "the queue refused the chain: {error:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Building a chain.
// ---------------------------------------------------------------------------

/// A byte range of a pinned region: `len` bytes from `offset`, in a region
/// whose page `i` the device reaches at `pages[i]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Data<'a> {
    /// The device address of each page, as `dma_pin` returned them.
    pub pages: &'a [u64],
    /// Where the data starts, in bytes from the region's start.
    pub offset: u64,
    /// How many bytes.
    pub len: u64,
}

/// The data descriptors of one chain, as [`plan`] laid them out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segments {
    /// The descriptors; only the first `count` mean anything.
    buffers: [Buffer; MAX_DATA_SEGMENTS],
    /// How many there are.
    count: usize,
    /// Their total length.
    bytes: u64,
}

impl Segments {
    /// No data: a flush.
    #[must_use]
    pub const fn none() -> Self {
        Segments {
            buffers: [Buffer::readable(0, 0); MAX_DATA_SEGMENTS],
            count: 0,
            bytes: 0,
        }
    }

    /// The data descriptors.
    #[must_use]
    pub fn buffers(&self) -> &[Buffer] {
        self.buffers.get(..self.count).unwrap_or(&[])
    }

    /// How many data descriptors.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Bytes of data the descriptors cover.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// The longest run of device-contiguous bytes from `at`, up to `stop` and to
/// `most`, as a device address and a length.
fn run(pages: &[u64], at: u64, stop: u64, most: u64) -> Result<(u64, u64), BlkError> {
    let page = usize::try_from(at / PAGE_SIZE).map_err(|_| BlkError::OutsideRegion)?;
    let within = at % PAGE_SIZE;
    let base = *pages.get(page).ok_or(BlkError::OutsideRegion)?;
    let address = base.checked_add(within).ok_or(BlkError::AddressOverflow)?;

    let mut len = (PAGE_SIZE - within).min(stop - at);
    let mut next = page;
    let mut expected = base;
    // Each step joins one more page, so the walk ends within the region.
    while len < most && at + len < stop {
        next += 1;
        expected = expected
            .checked_add(PAGE_SIZE)
            .ok_or(BlkError::AddressOverflow)?;
        match pages.get(next) {
            Some(&address) if address == expected => {
                len += PAGE_SIZE.min(stop - (at + len));
            }
            _ => break,
        }
    }
    let len = len.min(most);
    let _ = address.checked_add(len).ok_or(BlkError::AddressOverflow)?;
    Ok((address, len))
}

/// Lay out the data descriptors for the longest prefix of `data` that is a
/// whole number of `unit`s and fits in `max_segments` descriptors of at most
/// `max_segment_size` bytes each.
///
/// Each descriptor covers bytes whose device addresses are consecutive: one
/// page, or several whose addresses follow on, cut at `max_segment_size`. A
/// chain's data is also held below four gigabytes, so that the used ring's
/// 32-bit `written` can describe it. The prefix may be all of `data`, in which
/// case [`Segments::bytes`] equals `data.len`; the rest belongs in another
/// chain.
///
/// # Errors
///
/// [`BlkError::NotWholeUnits`] if `data.len` is not a multiple of `unit`,
/// [`BlkError::OutsideRegion`] if it reaches past the pages,
/// [`BlkError::AddressOverflow`] if a page's address plus a length does, and
/// [`BlkError::Unsplittable`] if not one unit fits.
pub fn plan(
    data: &Data<'_>,
    device_writable: bool,
    unit: u32,
    max_segments: usize,
    max_segment_size: u32,
) -> Result<Segments, BlkError> {
    let unit = u64::from(unit.max(1));
    if !data.len.is_multiple_of(unit) {
        return Err(BlkError::NotWholeUnits);
    }
    let region = u64::try_from(data.pages.len())
        .ok()
        .and_then(|pages| pages.checked_mul(PAGE_SIZE))
        .ok_or(BlkError::OutsideRegion)?;
    let end = data
        .offset
        .checked_add(data.len)
        .ok_or(BlkError::OutsideRegion)?;
    if end > region {
        return Err(BlkError::OutsideRegion);
    }

    let most_bytes = (u64::from(u32::MAX) - u64::from(STATUS_LEN)) / unit * unit;
    let stop = data.offset + data.len.min(most_bytes);
    let limit = max_segments.min(MAX_DATA_SEGMENTS);
    let most = u64::from(max_segment_size.max(1));

    let mut segments = Segments::none();
    let mut at = data.offset;
    while at < stop && segments.count < limit {
        let (address, len) = run(data.pages, at, stop, most)?;
        let buffer = Buffer {
            address,
            // At most `most`, which came from a `u32`.
            len: len as u32,
            device_writable,
        };
        if let Some(slot) = segments.buffers.get_mut(segments.count) {
            *slot = buffer;
        }
        segments.count += 1;
        at += len;
    }

    let whole = (at - data.offset) / unit * unit;
    if whole == 0 && data.len != 0 {
        return Err(BlkError::Unsplittable);
    }
    trim(&mut segments, whole);
    Ok(segments)
}

/// Cut `segments` down to exactly `bytes`, dropping descriptors past it and
/// shortening the last one kept.
fn trim(segments: &mut Segments, bytes: u64) {
    let mut kept: u64 = 0;
    let mut count = 0;
    for buffer in segments.buffers.iter_mut().take(segments.count) {
        if kept >= bytes {
            break;
        }
        let room = bytes - kept;
        if u64::from(buffer.len) > room {
            // Below `buffer.len`, so it fits.
            buffer.len = room as u32;
        }
        kept += u64::from(buffer.len);
        count += 1;
    }
    segments.count = count;
    segments.bytes = kept;
}

/// A chain [`publish`] put on the queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chain {
    /// The head descriptor, which the device names on completion.
    pub head: u16,
    /// Descriptors in the chain.
    pub descriptors: u16,
    /// Bytes of data in the chain.
    pub data_len: u32,
    /// Bytes the device may write: the data if it writes it, and the status.
    pub writable: u32,
}

/// Publish one request: a header the device reads at `header`, the data
/// `segments` lays out, and a status byte the device writes at `status`.
///
/// The caller has already written the [`HEADER_LEN`] header bytes at
/// `header`, because the device may read them the moment this returns. The
/// device learns of the chain when it is next notified.
///
/// # Errors
///
/// [`BlkError::AddressOverflow`] if `header` or `status` has no room for its
/// bytes below 2^64, and [`BlkError::Queue`] if the queue refuses the chain —
/// [`QueueError::OutOfDescriptors`] when it is full.
pub fn publish<M: QueueMemory>(
    queue: &mut SplitQueue<M>,
    header: u64,
    segments: &Segments,
    status: u64,
) -> Result<Chain, BlkError> {
    let _ = header
        .checked_add(u64::from(HEADER_LEN))
        .ok_or(BlkError::AddressOverflow)?;
    let _ = status
        .checked_add(u64::from(STATUS_LEN))
        .ok_or(BlkError::AddressOverflow)?;

    let mut buffers = [Buffer::readable(0, 0); MAX_CHAIN];
    let data = segments.buffers();
    let count = data.len() + 2;
    let mut slots = buffers.iter_mut();
    if let Some(slot) = slots.next() {
        *slot = Buffer::readable(header, HEADER_LEN);
    }
    // Driven by the data, not the slots: `slots.zip(data)` would take one slot
    // more than the data fills before noticing the data had run out, and the
    // status would land past the end of the chain.
    for buffer in data {
        if let Some(slot) = slots.next() {
            *slot = *buffer;
        }
    }
    if let Some(slot) = slots.next() {
        *slot = Buffer::writable(status, STATUS_LEN);
    }

    let writes_data = data.first().is_some_and(|buffer| buffer.device_writable);
    // `plan` holds a chain's data below `u32::MAX`, less the status byte.
    let data_len = u32::try_from(segments.bytes()).unwrap_or(u32::MAX - STATUS_LEN);
    let writable = if writes_data { data_len } else { 0 } + STATUS_LEN;

    let chain = buffers.get(..count).ok_or(BlkError::Unsplittable)?;
    let head = queue.add_chain(chain)?;
    Ok(Chain {
        head,
        // At most `MAX_CHAIN`.
        descriptors: count as u16,
        data_len,
        writable,
    })
}

/// What a completed chain says: the status byte, checked against the chain it
/// came back on.
///
/// `written` is only bounded above. Virtio 1.2 §2.7.8 has a device report the
/// bytes it wrote, but drivers — Linux's among them — have long ignored the
/// number, so devices have long got it wrong low; what can never be right is a
/// number above what the chain could hold, which says the device wrote
/// somewhere it was not given.
///
/// # Errors
///
/// [`BlkError::WrittenTooLong`] and [`BlkError::BadStatus`].
pub const fn parse_completion(chain: &Chain, written: u32, status: u8) -> Result<Status, BlkError> {
    if written > chain.writable {
        return Err(BlkError::WrittenTooLong {
            written,
            writable: chain.writable,
        });
    }
    Status::from_byte(status)
}
