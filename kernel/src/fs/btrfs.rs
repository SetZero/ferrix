//! btrfs, mounted from a disk a ring-3 driver serves.
//!
//! `mount -t btrfs /dev/vda /mnt` names a block node. Opening a block node is
//! `ENXIO` here, so the mount does not open it: it takes the node's `st_rdev`
//! and asks devfs's registry for the [`BlockDevice`] behind that number, the
//! one the block ring's kernel side registered when the driver said hello.
//! Everything above that is `ferrix-btrfs-vfs`, which reads the volume through
//! a [`Device`] — bytes at physical offsets — and keeps file data in the pages
//! the kernel's [`VmoStorage`] lends it. [`Disk`] is the adapter between the
//! two: whole sectors on one side, byte offsets on the other.
//!
//! # Read-only
//!
//! Stage 11 is btrfs stage A, the read path. A mount that does not ask for
//! `MS_RDONLY` is refused with `EROFS`, so no program is told it has a
//! writable btrfs until stage 12 makes one; `syscall::fsctl` judges the flag
//! and this module never sees a mount that could write.
//!
//! # The device outlives its registration
//!
//! The registry hands out an `Arc`, and the mount keeps it for its life. A
//! disk whose driver has died answers `EIO` to every read, promptly, and the
//! mount then answers `EIO` to every read of a file — never a hang and never
//! a panic — until it is unmounted.

use alloc::sync::Arc;
use alloc::vec;
use core::fmt;

use ferrix_btrfs::BtrfsError;
use ferrix_btrfs::volume::{Device, ReadKind};
use ferrix_btrfs_vfs::Btrfs;
use ferrix_vfs::{Errno, FileSystem};

use super::block::BlockDevice;
use super::devfs;
use super::pages::VmoStorage;

/// A registered disk as a btrfs volume reads it: bytes at physical offsets,
/// fetched in whole sectors.
///
/// Cloned for every operation, as `ferrix-btrfs-vfs` asks of its handle; a
/// clone is one `Arc` and the sector size.
#[derive(Clone)]
pub(crate) struct Disk {
    device: Arc<dyn BlockDevice>,
    /// Bytes in a sector: a power of two, checked once.
    sector: u64,
}

impl Disk {
    /// A handle on `device`.
    ///
    /// # Errors
    ///
    /// `EINVAL` for a sector size that is not a power of two, which no sector
    /// arithmetic below could serve.
    pub(crate) fn new(device: Arc<dyn BlockDevice>) -> Result<Disk, Errno> {
        let sector = u64::from(device.sector_size());
        if sector == 0 || !sector.is_power_of_two() {
            return Err(Errno::EINVAL);
        }
        Ok(Disk { device, sector })
    }
}

impl fmt::Debug for Disk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Disk")
            .field("sector", &self.sector)
            .field("sectors", &self.device.sectors())
            .finish_non_exhaustive()
    }
}

impl Device for Disk {
    /// Read `buf.len()` bytes at `physical`.
    ///
    /// A read that starts and ends on a sector boundary goes straight into
    /// `buf`, which is every read btrfs makes on a disk whose sectors are no
    /// larger than its own: nodes and extents sit on sector boundaries. One
    /// that does not is read into a bounce buffer of whole sectors and copied
    /// out, so the trait's contract holds on any sector size. Any failure,
    /// including a read past the end of the disk, is `DeviceRead` at the
    /// offset asked for; the volume reader turns that into `EIO`.
    fn read_at(
        &mut self,
        physical: u64,
        buf: &mut [u8],
        _kind: ReadKind,
    ) -> Result<(), BtrfsError> {
        let failed = BtrfsError::DeviceRead { physical };
        if buf.is_empty() {
            return Ok(());
        }
        let len = buf.len() as u64;
        let end = physical.checked_add(len).ok_or(failed)?;
        let first = physical / self.sector;
        if physical.is_multiple_of(self.sector) && len.is_multiple_of(self.sector) {
            return self.device.read(first, buf).map_err(|_| failed);
        }
        let sectors = end.div_ceil(self.sector).saturating_sub(first);
        let bytes = usize::try_from(sectors.saturating_mul(self.sector)).map_err(|_| failed)?;
        let mut bounce = vec![0u8; bytes];
        self.device.read(first, &mut bounce).map_err(|_| failed)?;
        let skip = usize::try_from(physical - first * self.sector).map_err(|_| failed)?;
        let taken = bounce.get(skip..skip + buf.len()).ok_or(failed)?;
        buf.copy_from_slice(taken);
        Ok(())
    }
}

/// Mount the btrfs volume on the registered disk numbered `rdev`, read-only.
///
/// The mount reports `rdev` as every inode's `st_dev`, as Linux reports the
/// block device a disk filesystem is on, and reads file data through the
/// kernel's VMO pages.
///
/// # Errors
///
/// `ENXIO` for a number no registered disk has, `EINVAL` for a disk that
/// holds no btrfs volume this reader reads (a log tree, an unknown feature,
/// a checksum type other than CRC-32C), and `EIO` for one that cannot be
/// read.
pub(crate) fn mount(rdev: u64) -> Result<Arc<dyn FileSystem>, Errno> {
    let device = devfs::block_device(rdev).ok_or(Errno::ENXIO)?;
    let disk = Disk::new(device)?;
    let volume = Btrfs::mount(disk, rdev, Arc::new(VmoStorage))?;
    Ok(volume as Arc<dyn FileSystem>)
}
