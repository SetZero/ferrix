//! What the kernel needs from a disk: whole sectors, read on request.
//!
//! Stage 11 mounts a btrfs volume from a virtio-blk disk served by a ring-3
//! driver, and `mount(2)` names that disk by a node in `/dev`. Opening a block
//! node is `ENXIO` here, as it is for every block number no driver answers, so
//! a mount does not open it: it resolves the node's `st_rdev` through the
//! registry in [`crate::fs::devfs`] and reads through the device that returns.
//! This trait is all that registry knows about a disk, which is what lets it
//! exist before stage 11's concrete kernel disk does.
//!
//! # Lifetime
//!
//! The registry hands out an `Arc`, and a mount keeps it. So a disk can
//! outlive its registration — its driver died, devmgr removed it, a reset is
//! pending — and a device in that state has to keep answering: every read
//! fails with `EIO`, promptly, and nothing panics.

use core::fmt;

use ferrix_vfs::Errno;

/// A disk: a run of equal-sized sectors that can be read.
pub(crate) trait BlockDevice: Send + Sync + fmt::Debug {
    /// Read whole sectors from `sector` into `buf`, whose length is a whole
    /// number of sectors. Called with no spin lock held; may sleep on the
    /// device. Past the end, or a device failure, is an error. A device that
    /// has gone away (driver dead, unregistered, reset pending) returns EIO:
    /// it never blocks forever and never panics — a mount may outlive the
    /// registration by holding the Arc.
    fn read(&self, sector: u64, buf: &mut [u8]) -> Result<(), Errno>;

    /// How many sectors the disk has.
    fn sectors(&self) -> u64;

    /// How many bytes a sector is.
    fn sector_size(&self) -> u32;

    /// Whether the disk refuses writes.
    fn read_only(&self) -> bool;
}

/// The disk's size in bytes, saturating rather than wrapping for a size no
/// `u64` holds.
pub(crate) fn size_in_bytes(device: &dyn BlockDevice) -> u64 {
    device
        .sectors()
        .saturating_mul(u64::from(device.sector_size()))
}
