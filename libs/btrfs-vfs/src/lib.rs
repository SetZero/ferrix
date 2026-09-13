//! btrfs, mounted: the VFS's [`FileSystem`] and [`Inode`] over `ferrix-btrfs`.
//!
//! `ferrix-btrfs` reads a volume through a [`Device`] into buffers its caller
//! supplies, and allocates nothing. The VFS wants `Arc<dyn Inode>` objects that
//! answer on their own. This crate is the part in between, and what it adds is
//! ownership, sharing, and the translation into Linux's answers.
//!
//! # Read-only
//!
//! Stage 11 is btrfs stage A. Every operation that would change the volume
//! answers `EROFS`, which is what Linux says on a read-only mount, rather than
//! the `ENOTDIR` or `EINVAL` the trait's defaults give an object of the wrong
//! kind.
//!
//! # No lock is held across I/O
//!
//! The [`Inode`] contract forbids holding a spin lock across I/O, and a spin
//! lock is all there is. So nothing that does I/O sits behind one:
//!
//! * the device is a handle, cloned for each operation, so how concurrent reads
//!   meet is the device's business — in the kernel, the block core's queue;
//! * the [`Volume`] does not change after mount and is shared freely;
//! * working memory comes from a pool whose lock is held to take a set of
//!   buffers and to give it back, never while the buffers are in use. An
//!   operation that finds the pool empty allocates a fresh set, so two reads
//!   never wait on each other's memory.
//!
//! Each pooled set keeps its decompression cache. On a read-only volume an
//! extent's bytes never change, so a cached extent stays correct whichever
//! inode reads it next.
//!
//! # Inodes are built at lookup
//!
//! An inode object is made from its `INODE_ITEM` when a lookup finds it, and
//! its metadata is fixed from then on, which is right for a volume nothing
//! writes. The VFS's dentry cache is what keeps it alive between lookups.
//!
//! # What is not crossed
//!
//! A directory entry naming another subvolume is left out of listings and
//! reported absent by lookup, so the two always agree; entering subvolumes is
//! btrfs stage C. Device numbers are passed through as btrfs stores them:
//! nothing in the test images is a device node, and how that value maps to
//! `st_rdev` has not yet been checked against Linux.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;
use core::ops::ControlFlow;

use ferrix_btrfs::BtrfsError;
use ferrix_btrfs::chunk::ChunkMapEntry;
use ferrix_btrfs::compress::MAX_UNCOMPRESSED;
use ferrix_btrfs::compress::zstd::Workspace;
use ferrix_btrfs::fs::{ExtentBuffers, ReadBuffers, Subvolume, Target};
use ferrix_btrfs::items::{self, InodeItem};
use ferrix_btrfs::tree::MAX_NODE_SIZE;
use ferrix_btrfs::volume::{Device, Volume};
use ferrix_sync::SpinLock;
use ferrix_vfs::{
    DirEntry, Errno, FIRST_CURSOR, FileSystem, FileType, Inode, Metadata, NewNode, Result,
    SetAttributes, StatFs, Timespec,
};

/// How many chunks a mounted volume may have.
///
/// Data chunks are a gibibyte each, so this is a volume of several terabytes,
/// for 160 KiB of map.
pub const MAX_CHUNKS: usize = 4096;

/// How many idle buffer sets a mount keeps. Each is about 0.4 MiB, so the pool
/// covers a few concurrent readers without holding memory for many.
const POOLED: usize = 4;

/// The longest symlink target Linux will store: `PATH_MAX` less its NUL.
const MAX_LINK: u64 = 4095;

/// `f_type` for btrfs, which programs compare against to learn what they are
/// running on.
pub const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;

/// The longest name a btrfs directory entry may have.
const NAME_MAX: u64 = 255;

/// What a mount reads through: a [`Device`] handle that can be cloned for
/// each operation and shared across threads.
pub trait BlockHandle: Device + Clone + Send + Sync + 'static {}

impl<T: Device + Clone + Send + Sync + 'static> BlockHandle for T {}

// ---------------------------------------------------------------------------
// Working memory
// ---------------------------------------------------------------------------

/// The extent buffers one set owns.
struct Owned {
    compressed: Box<[u8]>,
    plain: Box<[u8]>,
    zstd: Box<[u8]>,
}

impl ExtentBuffers for Owned {
    fn parts(&mut self) -> (&mut [u8], &mut [u8], &mut [u8]) {
        (&mut self.compressed, &mut self.plain, &mut self.zstd)
    }
}

impl fmt::Debug for Owned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Owned").finish_non_exhaustive()
    }
}

/// Everything one operation reads into: a node buffer and the extent buffers.
#[derive(Debug)]
struct Scratch {
    node: Box<[u8]>,
    read: ReadBuffers<Owned>,
}

impl Scratch {
    fn new() -> Result<Scratch> {
        let owned = Owned {
            compressed: zeroed(MAX_UNCOMPRESSED),
            plain: zeroed(MAX_UNCOMPRESSED),
            zstd: zeroed(Workspace::SIZE),
        };
        Ok(Scratch {
            node: zeroed(MAX_NODE_SIZE as usize),
            read: ReadBuffers::new(owned).map_err(errno)?,
        })
    }
}

/// A heap buffer of `len` zeroes, built on the heap rather than moved there.
fn zeroed(len: usize) -> Box<[u8]> {
    vec![0u8; len].into_boxed_slice()
}

// ---------------------------------------------------------------------------
// The mount
// ---------------------------------------------------------------------------

/// What every inode of one mount shares.
struct Shared<D> {
    volume: Volume<Box<[ChunkMapEntry]>>,
    device: D,
    dev_no: u64,
    pool: SpinLock<Vec<Scratch>>,
}

impl<D: BlockHandle> Shared<D> {
    /// Run `op` with a device handle and a buffer set, and translate its error.
    fn with<R>(
        &self,
        op: impl FnOnce(
            &Subvolume<'_, Box<[ChunkMapEntry]>>,
            &mut D,
            &mut Scratch,
        ) -> core::result::Result<R, BtrfsError>,
    ) -> Result<R> {
        let taken = self.pool.lock().pop();
        let mut scratch = match taken {
            Some(scratch) => scratch,
            None => Scratch::new()?,
        };
        let mut device = self.device.clone();
        let result = op(&self.volume.default_subvolume(), &mut device, &mut scratch);
        let mut pool = self.pool.lock();
        if pool.len() < POOLED {
            pool.push(scratch);
        }
        drop(pool);
        result.map_err(errno)
    }
}

impl<D> fmt::Debug for Shared<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("dev_no", &self.dev_no)
            .field("nodesize", &self.volume.nodesize())
            .finish_non_exhaustive()
    }
}

/// A mounted btrfs volume.
pub struct Btrfs<D> {
    shared: Arc<Shared<D>>,
    root: Arc<Node<D>>,
}

impl<D: BlockHandle> Btrfs<D> {
    /// Mount the volume on `device`, reporting `dev_no` as every inode's device.
    ///
    /// `EINVAL` when the device does not hold a btrfs volume this reader will
    /// read, and `EIO` when it does but cannot be read.
    pub fn mount(device: D, dev_no: u64) -> Result<Arc<Btrfs<D>>> {
        let mut reader = device.clone();
        let chunks = vec![ChunkMapEntry::EMPTY; MAX_CHUNKS].into_boxed_slice();
        let mut scratch = Scratch::new()?;
        let volume = Volume::open(&mut reader, chunks, &mut scratch.node).map_err(mount_errno)?;
        let shared = Arc::new(Shared {
            volume,
            device,
            dev_no,
            pool: SpinLock::new(vec![scratch]),
        });
        let root = Node::load(&shared, shared.volume.root_dir())?;
        if root.meta.kind != FileType::Directory {
            return Err(Errno::EIO);
        }
        Ok(Arc::new(Btrfs { shared, root }))
    }
}

impl<D> fmt::Debug for Btrfs<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Btrfs")
            .field("shared", &self.shared)
            .finish_non_exhaustive()
    }
}

impl<D: BlockHandle> FileSystem for Btrfs<D> {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::clone(&self.root) as Arc<dyn Inode>
    }

    fn name(&self) -> &'static str {
        "btrfs"
    }

    fn device(&self) -> u64 {
        self.shared.dev_no
    }

    /// Sizes from the superblock, in sectors.
    ///
    /// Linux derives free space from each block group's space info, which
    /// would mean walking the extent tree at every `statfs`; total less used,
    /// as the superblock records them, is an approximation of that figure,
    /// and on a read-only mount nothing can spend it. btrfs has no inode
    /// table, so, as on Linux, the inode counts are zero.
    fn statfs(&self) -> StatFs {
        let volume = &self.shared.volume;
        let block_size = u64::from(volume.sectorsize());
        let free = volume.total_bytes().saturating_sub(volume.bytes_used()) / block_size;
        StatFs {
            magic: BTRFS_SUPER_MAGIC,
            block_size,
            blocks: volume.total_bytes() / block_size,
            blocks_free: free,
            blocks_available: free,
            files: 0,
            files_free: 0,
            name_max: NAME_MAX,
        }
    }
}

// ---------------------------------------------------------------------------
// Inodes
// ---------------------------------------------------------------------------

/// One file, directory or link of a mounted volume.
struct Node<D> {
    shared: Arc<Shared<D>>,
    meta: Metadata,
}

impl<D: BlockHandle> Node<D> {
    /// Build the inode object for `ino` from its `INODE_ITEM`.
    fn load(shared: &Arc<Shared<D>>, ino: u64) -> Result<Arc<Node<D>>> {
        let item = shared
            .with(|sub, device, scratch| sub.inode(device, ino, &mut scratch.node))?
            // A name that leads to no inode is a damaged tree, not a miss.
            .ok_or(Errno::EIO)?;
        let meta = metadata(ino, &item, shared.volume.sectorsize())?;
        Ok(Arc::new(Node {
            shared: Arc::clone(shared),
            meta,
        }))
    }

    fn require_dir(&self) -> Result<()> {
        if self.meta.kind == FileType::Directory {
            Ok(())
        } else {
            Err(Errno::ENOTDIR)
        }
    }

    /// The answer to any change asked of a directory: the name would be
    /// valid, but the volume is read-only.
    fn read_only<T>(&self) -> Result<T> {
        self.require_dir()?;
        Err(Errno::EROFS)
    }
}

impl<D> fmt::Debug for Node<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("ino", &self.meta.ino)
            .field("kind", &self.meta.kind)
            .finish_non_exhaustive()
    }
}

impl<D: BlockHandle> Inode for Node<D> {
    fn metadata(&self) -> Metadata {
        self.meta
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn set_attributes(&self, change: &SetAttributes) -> Result<()> {
        let _ = change;
        Err(Errno::EROFS)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if self.meta.kind != FileType::Regular {
            return Err(Errno::EINVAL);
        }
        let ino = self.meta.ino;
        self.shared.with(|sub, device, scratch| {
            sub.read(
                device,
                ino,
                offset,
                buf,
                &mut scratch.node,
                &mut scratch.read,
            )
        })
    }

    fn write_at(&self, offset: u64, data: &[u8], append: bool) -> Result<(usize, u64)> {
        let _ = (offset, data, append);
        Err(Errno::EROFS)
    }

    fn set_len(&self, len: u64) -> Result<()> {
        let _ = len;
        Err(Errno::EROFS)
    }

    fn lookup(&self, name: &[u8]) -> Result<Arc<dyn Inode>> {
        self.require_dir()?;
        let dir = self.meta.ino;
        let found = self
            .shared
            .with(|sub, device, scratch| sub.lookup(device, dir, name, &mut scratch.node))?;
        match found.map(|entry| entry.target) {
            Some(Target::Inode(ino)) => Ok(Node::load(&self.shared, ino)? as Arc<dyn Inode>),
            Some(Target::Subvolume(_)) | None => Err(Errno::ENOENT),
        }
    }

    fn create(&self, name: &[u8], node: NewNode<'_>, permissions: u32) -> Result<Arc<dyn Inode>> {
        let _ = (name, node, permissions);
        self.read_only()
    }

    fn link(&self, name: &[u8], target: &Arc<dyn Inode>) -> Result<()> {
        let _ = (name, target);
        self.read_only()
    }

    fn unlink(&self, name: &[u8]) -> Result<()> {
        let _ = name;
        self.read_only()
    }

    fn rmdir(&self, name: &[u8]) -> Result<()> {
        let _ = name;
        self.read_only()
    }

    fn rename(
        &self,
        old: &[u8],
        new_parent: &Arc<dyn Inode>,
        new: &[u8],
        replace: bool,
    ) -> Result<()> {
        let _ = (old, new_parent, new, replace);
        self.read_only()
    }

    fn read_dir(&self, cursor: u64, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
        self.require_dir()?;
        let dir = self.meta.ino;
        let mut damaged = false;
        self.shared.with(|sub, device, scratch| {
            let from = cursor.max(FIRST_CURSOR);
            sub.read_dir(device, dir, from, &mut scratch.node, |entry| {
                let Target::Inode(ino) = entry.target else {
                    return ControlFlow::Continue(());
                };
                let Some(kind) = entry_kind(entry.kind) else {
                    damaged = true;
                    return ControlFlow::Break(());
                };
                let listed = DirEntry {
                    ino,
                    kind,
                    name: entry.name,
                    next: entry.index.saturating_add(1),
                };
                if emit(listed) {
                    ControlFlow::Continue(())
                } else {
                    ControlFlow::Break(())
                }
            })
        })?;
        if damaged { Err(Errno::EIO) } else { Ok(()) }
    }

    fn read_link(&self) -> Result<Vec<u8>> {
        if self.meta.kind != FileType::Symlink {
            return Err(Errno::EINVAL);
        }
        if self.meta.size == 0 || self.meta.size > MAX_LINK {
            return Err(Errno::EIO);
        }
        let len = usize::try_from(self.meta.size).map_err(|_| Errno::EIO)?;
        let mut target = vec![0u8; len];
        let ino = self.meta.ino;
        let read = self.shared.with(|sub, device, scratch| {
            sub.read(
                device,
                ino,
                0,
                &mut target,
                &mut scratch.node,
                &mut scratch.read,
            )
        })?;
        if read == len {
            Ok(target)
        } else {
            Err(Errno::EIO)
        }
    }
}

// ---------------------------------------------------------------------------
// Translation
// ---------------------------------------------------------------------------

/// What `stat` reports for an `INODE_ITEM`.
fn metadata(ino: u64, item: &InodeItem, block_size: u32) -> Result<Metadata> {
    let kind = FileType::from_mode(item.mode).ok_or(Errno::EIO)?;
    Ok(Metadata {
        ino,
        kind,
        permissions: item.mode & 0o7777,
        nlink: item.nlink,
        uid: item.uid,
        gid: item.gid,
        size: item.size,
        rdev: item.rdev,
        blocks: item.nbytes / 512,
        block_size,
        atime: time(item.atime),
        mtime: time(item.mtime),
        ctime: time(item.ctime),
    })
}

/// A btrfs timestamp as Linux reports it. The kernel reads the seconds as
/// signed, so the bits are reinterpreted rather than range-checked.
fn time(stamp: items::Timespec) -> Timespec {
    Timespec {
        tv_sec: i64::from_ne_bytes(stamp.sec.to_ne_bytes()),
        tv_nsec: i64::from(stamp.nsec.min(999_999_999)),
    }
}

/// The file type a directory entry records.
fn entry_kind(kind: u8) -> Option<FileType> {
    match kind {
        items::FT_REG_FILE => Some(FileType::Regular),
        items::FT_DIR => Some(FileType::Directory),
        items::FT_SYMLINK => Some(FileType::Symlink),
        items::FT_CHRDEV => Some(FileType::CharDevice),
        items::FT_BLKDEV => Some(FileType::BlockDevice),
        items::FT_FIFO => Some(FileType::Fifo),
        items::FT_SOCK => Some(FileType::Socket),
        _ => None,
    }
}

/// The error an operation on a mounted volume reports. Anything wrong with the
/// bytes, or with reading them, is `EIO`: by now the volume was accepted, so a
/// bad node is damage, not a question of what the device holds.
const fn errno(error: BtrfsError) -> Errno {
    let _ = error;
    Errno::EIO
}

/// The error a mount reports: `EINVAL` for a device that is not a volume this
/// reader accepts, as Linux's `mount` does, and `EIO` for one that is but
/// cannot be read.
const fn mount_errno(error: BtrfsError) -> Errno {
    match error {
        BtrfsError::BadMagic
        | BtrfsError::UnsupportedChecksum(_)
        | BtrfsError::UnsupportedFeature(_)
        | BtrfsError::MultipleDevices(_)
        | BtrfsError::UnreplayedLog
        | BtrfsError::UnsupportedProfile(_)
        | BtrfsError::BadSectorSize(_)
        | BtrfsError::BadNodeSize(_) => Errno::EINVAL,
        _ => Errno::EIO,
    }
}

#[cfg(test)]
mod tests;
