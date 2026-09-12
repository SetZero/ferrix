//! Files and directories, as a read-only filesystem is asked for them.
//!
//! Inside one fs tree an inode number is an object id, and everything about
//! that inode sorts together under it: the `INODE_ITEM` with its stat data,
//! the `INODE_REF`s naming it, and — for a directory — its entries, or — for a
//! file or symlink — its `EXTENT_DATA` items keyed by file offset.
//!
//! # A directory holds each entry twice
//!
//! Once as a `DIR_ITEM` keyed by the CRC-32C of the name, which is what makes
//! a lookup one seek; and once as a `DIR_INDEX` keyed by a sequence number
//! that only grows, which is what makes `readdir` resumable from a cursor that
//! stays valid while entries are added and removed. [`Subvolume::lookup`] uses
//! the first and [`Subvolume::read_dir`] the second. Index numbers start at 2,
//! leaving 0 and 1 to the `.` and `..` a VFS emits itself.
//!
//! # A file is extents, and gaps
//!
//! Each `EXTENT_DATA` covers a range of the file starting at its key's offset.
//! A range with no item is a hole when the volume has `NO_HOLES`, and so is a
//! regular extent with a zero disk address, and so is a preallocated extent:
//! all three read as zeroes. A read therefore zero-fills first and copies the
//! extents it finds over the top, which handles every hole the same way and
//! cannot leave stale bytes in a range nothing covered.
//!
//! # Compressed extents are decompressed whole
//!
//! A compressed extent cannot be entered in the middle, so reading 4 KiB of it
//! means expanding up to 128 KiB. [`ReadBuffers`] keeps the last extent it
//! expanded, which turns a sequential read through a compressed file from one
//! decompression per call into one per extent.
//!
//! # Subvolumes are named, not entered
//!
//! A directory entry can point at the root of another subvolume. This reader
//! reports that as [`Target::Subvolume`] and goes no further: crossing into it
//! is a mount decision, and stage C's.

use core::ops::ControlFlow;

use crate::chunk::ChunkStorage;
use crate::compress::{self, MAX_UNCOMPRESSED};
use crate::items::{
    DIR_INDEX_KEY, DIR_ITEM_KEY, DirItem, DirItemIter, EXTENT_DATA_KEY, ExtentData, ExtentDataBody,
    FileExtent, INODE_ITEM_KEY, InodeItem, ROOT_ITEM_KEY, name_hash,
};
use crate::tree::BtrfsKey;
use crate::volume::{Device, TreeRoot, Volume};
use crate::{BtrfsError, truncated};

/// What a directory entry points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// An inode in the same tree, by number.
    Inode(u64),
    /// The root of another subvolume, by tree id. Not crossed by this reader.
    Subvolume(u64),
}

/// A directory entry found by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// What the name refers to.
    pub target: Target,
    /// The entry's file type, one of the `FT_*` constants in
    /// [`crate::items`], recorded so a caller need not read the inode.
    pub kind: u8,
}

/// A directory entry produced by [`Subvolume::read_dir`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry<'a> {
    /// The entry's `DIR_INDEX` sequence number. Resuming a listing at
    /// `index + 1` continues after this entry.
    pub index: u64,
    /// The name, as raw bytes.
    pub name: &'a [u8],
    /// What the name refers to.
    pub target: Target,
    /// The entry's file type, one of the `FT_*` constants.
    pub kind: u8,
}

/// The memory a compressed extent is read and expanded in.
///
/// Two buffers of at least [`MAX_UNCOMPRESSED`] bytes and the zstd workspace,
/// handed out as three disjoint borrows. A tuple of borrows is one; a mount
/// implements it over buffers it owns.
pub trait ExtentBuffers {
    /// The buffer compressed bytes are read into, the buffer they expand
    /// into, and zstd's workspace.
    fn parts(&mut self) -> (&mut [u8], &mut [u8], &mut compress::zstd::Workspace);
}

impl ExtentBuffers for (&mut [u8], &mut [u8], &mut compress::zstd::Workspace) {
    fn parts(&mut self) -> (&mut [u8], &mut [u8], &mut compress::zstd::Workspace) {
        (&mut *self.0, &mut *self.1, &mut *self.2)
    }
}

/// The working memory a file read needs beyond the node buffer.
///
/// Two 128 KiB buffers and the zstd workspace: too large for a kernel stack,
/// and worth keeping between reads because of the extent cache. It owns its
/// [`ExtentBuffers`] rather than borrowing them, so a mount can keep one
/// across calls: rebuilt around borrowed buffers on every call, the cache
/// would never hit, and carrying the cache over to buffers it did not fill
/// would hand back somebody else's bytes.
#[derive(Debug)]
pub struct ReadBuffers<B> {
    buffers: B,
    cached: Option<Expanded>,
}

/// Which extent `plain` currently holds, and how much of it decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Expanded {
    disk_bytenr: u64,
    disk_num_bytes: u64,
    compression: u8,
    len: usize,
}

impl<B: ExtentBuffers> ReadBuffers<B> {
    /// Take over caller-allocated memory. Both byte buffers must hold at least
    /// [`MAX_UNCOMPRESSED`] bytes.
    pub fn new(mut buffers: B) -> Result<Self, BtrfsError> {
        let (compressed, plain, _) = buffers.parts();
        let shortest = compressed.len().min(plain.len());
        if shortest < MAX_UNCOMPRESSED {
            return Err(truncated(MAX_UNCOMPRESSED, shortest));
        }
        Ok(ReadBuffers {
            buffers,
            cached: None,
        })
    }

    /// Expand compressed bytes that live in the item itself.
    fn expand_inline(
        &mut self,
        compression: u8,
        data: &[u8],
        ram_bytes: u64,
        sectorsize: u32,
    ) -> Result<&[u8], BtrfsError> {
        // The buffer is about to be overwritten, so whatever it cached is gone.
        self.cached = None;
        let (_, plain, zstd) = self.buffers.parts();
        let out = plain
            .get_mut(..expanded_len(ram_bytes)?)
            .unwrap_or_default();
        let len = compress::decompress(compression, data, out, sectorsize, zstd)?;
        Ok(plain.get(..len).unwrap_or_default())
    }

    /// Expand a compressed regular extent, or reuse it if it is the one held.
    fn expand_extent<D: Device, S: ChunkStorage>(
        &mut self,
        volume: &Volume<S>,
        device: &mut D,
        extent: &ExtentData<'_>,
        file: &FileExtent,
    ) -> Result<&[u8], BtrfsError> {
        let wanted = Expanded {
            disk_bytenr: file.disk_bytenr,
            disk_num_bytes: file.disk_num_bytes,
            compression: extent.compression,
            len: 0,
        };
        let hit = self
            .cached
            .filter(|held| Expanded { len: 0, ..*held } == wanted);
        let (compressed, plain, zstd) = self.buffers.parts();
        let len = match hit {
            Some(held) => held.len,
            None => {
                // Forgotten before the read, so a failed read or decode cannot
                // leave the cache naming bytes the buffer no longer holds.
                self.cached = None;
                let stored = usize::try_from(file.disk_num_bytes)
                    .ok()
                    .filter(|&n| n <= MAX_UNCOMPRESSED)
                    .ok_or(BtrfsError::BadItem {
                        item_type: EXTENT_DATA_KEY,
                    })?;
                let input = compressed.get_mut(..stored).unwrap_or_default();
                volume.read_logical(device, file.disk_bytenr, input)?;
                let out = plain
                    .get_mut(..expanded_len(extent.ram_bytes)?)
                    .unwrap_or_default();
                let input = compressed.get(..stored).unwrap_or_default();
                let len = compress::decompress(
                    extent.compression,
                    input,
                    out,
                    volume.sectorsize(),
                    zstd,
                )?;
                self.cached = Some(Expanded { len, ..wanted });
                len
            }
        };
        Ok(plain.get(..len).unwrap_or_default())
    }
}

/// One subvolume's tree, read through a [`Volume`].
#[derive(Debug)]
pub struct Subvolume<'v, S> {
    volume: &'v Volume<S>,
    tree: TreeRoot,
    root_dir: u64,
}

// Written out rather than derived: a derive would demand `S: Copy`, and a
// `Subvolume` is only a reference and two numbers whatever `S` is.
impl<S> Clone for Subvolume<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> Copy for Subvolume<'_, S> {}

impl<S: ChunkStorage> Volume<S> {
    /// The subvolume mounted by default: the top-level fs tree.
    #[must_use]
    pub const fn default_subvolume(&self) -> Subvolume<'_, S> {
        Subvolume {
            volume: self,
            tree: self.fs_tree(),
            root_dir: self.root_dir(),
        }
    }
}

impl<S: ChunkStorage> Subvolume<'_, S> {
    /// Inode number of the subvolume's root directory.
    #[must_use]
    pub const fn root_dir(&self) -> u64 {
        self.root_dir
    }

    /// The stat data of inode `ino`, or `None` if the tree has no such inode.
    pub fn inode<D: Device>(
        &self,
        device: &mut D,
        ino: u64,
        node: &mut [u8],
    ) -> Result<Option<InodeItem>, BtrfsError> {
        let key = BtrfsKey::new(ino, INODE_ITEM_KEY, 0);
        let found = self.volume.walk(device, self.tree, key, node, |_, item| {
            if item.key != key {
                return Ok(ControlFlow::Break(None));
            }
            Ok(ControlFlow::Break(Some(InodeItem::parse(item.data)?)))
        })?;
        Ok(found.flatten())
    }

    /// Find `name` in directory `dir`.
    ///
    /// `None` is an ordinary miss. Names that collide under the hash share one
    /// `DIR_ITEM`, so every entry in it is compared.
    pub fn lookup<D: Device>(
        &self,
        device: &mut D,
        dir: u64,
        name: &[u8],
        node: &mut [u8],
    ) -> Result<Option<Entry>, BtrfsError> {
        let key = BtrfsKey::new(dir, DIR_ITEM_KEY, name_hash(name));
        let found = self.volume.walk(device, self.tree, key, node, |_, item| {
            if item.key != key {
                return Ok(ControlFlow::Break(None));
            }
            Ok(ControlFlow::Break(entry_named(item.data, name)?))
        })?;
        Ok(found.flatten())
    }

    /// List directory `dir` from sequence number `from`, handing each entry to
    /// `emit` until it breaks or the directory ends.
    pub fn read_dir<D: Device>(
        &self,
        device: &mut D,
        dir: u64,
        from: u64,
        node: &mut [u8],
        mut emit: impl FnMut(DirEntry<'_>) -> ControlFlow<()>,
    ) -> Result<(), BtrfsError> {
        let start = BtrfsKey::new(dir, DIR_INDEX_KEY, from);
        // Whether the listing ended or `emit` stopped it is the caller's to know.
        let _: Option<()> = self
            .volume
            .walk(device, self.tree, start, node, |_, item| {
                if item.key.objectid != dir || item.key.item_type != DIR_INDEX_KEY {
                    return Ok(ControlFlow::Break(()));
                }
                // A `DIR_INDEX` holds exactly one entry; hash collisions only ever
                // share a `DIR_ITEM`.
                let Some(entry) = DirItemIter::new(item.data, DIR_INDEX_KEY)
                    .next()
                    .transpose()?
                else {
                    return Err(BtrfsError::BadItem {
                        item_type: DIR_INDEX_KEY,
                    });
                };
                Ok(emit(DirEntry {
                    index: item.key.offset,
                    name: entry.name,
                    target: target_of(&entry)?,
                    kind: entry.kind,
                }))
            })?;
        Ok(())
    }

    /// Read file `ino` from byte `offset` into `out`, returning the length.
    ///
    /// Stops at end of file, so a short count means end of file and nothing
    /// else. A symlink's target is read the same way, from offset zero.
    pub fn read<D: Device, B: ExtentBuffers>(
        &self,
        device: &mut D,
        ino: u64,
        offset: u64,
        out: &mut [u8],
        node: &mut [u8],
        buffers: &mut ReadBuffers<B>,
    ) -> Result<usize, BtrfsError> {
        let inode = self
            .inode(device, ino, node)?
            .ok_or(BtrfsError::MissingInode(ino))?;
        let remaining = inode.size.saturating_sub(offset);
        let len = usize::try_from(remaining).map_or(out.len(), |r| r.min(out.len()));
        let out = out.get_mut(..len).unwrap_or_default();
        out.fill(0);
        if len == 0 {
            return Ok(0);
        }
        let end = offset.saturating_add(len as u64);
        let probe = BtrfsKey::new(ino, EXTENT_DATA_KEY, offset);
        let start = self
            .volume
            .last_at_or_before(device, self.tree, &probe, node)?
            .filter(|key| key.objectid == ino && key.item_type == EXTENT_DATA_KEY)
            .unwrap_or(BtrfsKey::new(ino, EXTENT_DATA_KEY, 0));
        // Every way out of the walk leaves `out` complete: extents past `end`
        // break it, and running out of items leaves the zero fill.
        let _: Option<()> = self
            .volume
            .walk(device, self.tree, start, node, |device, item| {
                let key = item.key;
                if key.objectid != ino || key.item_type != EXTENT_DATA_KEY || key.offset >= end {
                    return Ok(ControlFlow::Break(()));
                }
                let extent = ExtentData::parse(item.data)?;
                let place = Placement::new(key.offset, &extent, offset, out.len());
                if let Some(place) = place {
                    self.copy_extent(device, &extent, place, out, buffers)?;
                }
                Ok(ControlFlow::Continue(()))
            })?;
        Ok(len)
    }

    /// Copy the part of one extent that `place` selects into `out`.
    fn copy_extent<D: Device, B: ExtentBuffers>(
        &self,
        device: &mut D,
        extent: &ExtentData<'_>,
        place: Placement,
        out: &mut [u8],
        buffers: &mut ReadBuffers<B>,
    ) -> Result<(), BtrfsError> {
        let dest = out
            .get_mut(place.dest_start..place.dest_end)
            .ok_or(BtrfsError::BadItem {
                item_type: EXTENT_DATA_KEY,
            })?;
        match extent.body {
            ExtentDataBody::Prealloc(_) => Ok(()),
            ExtentDataBody::Regular(file) if file.is_hole() => Ok(()),
            ExtentDataBody::Inline(data) if extent.is_uncompressed() => {
                copy_from(dest, data, place.skip);
                Ok(())
            }
            ExtentDataBody::Inline(data) => {
                let sectorsize = self.volume.sectorsize();
                let plain = buffers.expand_inline(
                    extent.compression,
                    data,
                    extent.ram_bytes,
                    sectorsize,
                )?;
                copy_from(dest, plain, place.skip);
                Ok(())
            }
            ExtentDataBody::Regular(file) if extent.is_uncompressed() => {
                let at = file
                    .disk_bytenr
                    .checked_add(file.offset)
                    .and_then(|a| a.checked_add(place.skip))
                    .ok_or(BtrfsError::NotMapped(file.disk_bytenr))?;
                self.volume.read_logical(device, at, dest)
            }
            ExtentDataBody::Regular(file) => {
                let plain = buffers.expand_extent(self.volume, device, extent, &file)?;
                copy_from(dest, plain, file.offset.saturating_add(place.skip));
                Ok(())
            }
        }
    }
}

/// Where one extent lands in a read.
#[derive(Debug, Clone, Copy)]
struct Placement {
    /// How far into the extent's file range the copied part begins.
    skip: u64,
    /// The copied part's range within the read buffer.
    dest_start: usize,
    dest_end: usize,
}

impl Placement {
    /// Intersect the extent starting at file offset `start` with a read of
    /// `len` bytes at `offset`. `None` when they do not overlap.
    fn new(start: u64, extent: &ExtentData<'_>, offset: u64, len: usize) -> Option<Placement> {
        let covers = match extent.body {
            ExtentDataBody::Inline(data) if extent.is_uncompressed() => data.len() as u64,
            ExtentDataBody::Inline(_) => extent.ram_bytes,
            ExtentDataBody::Regular(file) | ExtentDataBody::Prealloc(file) => file.num_bytes,
        };
        let from = start.max(offset);
        let to = start
            .saturating_add(covers)
            .min(offset.saturating_add(len as u64));
        if from >= to {
            return None;
        }
        Some(Placement {
            skip: from.checked_sub(start)?,
            dest_start: usize::try_from(from.checked_sub(offset)?).ok()?,
            dest_end: usize::try_from(to.checked_sub(offset)?).ok()?,
        })
    }
}

/// Copy `src[at..]` into the front of `dest`, as much as both have. Whatever
/// is not covered stays as it was, which for a read is zero.
fn copy_from(dest: &mut [u8], src: &[u8], at: u64) {
    let tail = usize::try_from(at)
        .ok()
        .and_then(|at| src.get(at..))
        .unwrap_or_default();
    let len = dest.len().min(tail.len());
    if let (Some(to), Some(from)) = (dest.get_mut(..len), tail.get(..len)) {
        to.copy_from_slice(from);
    }
}

/// The buffer length a compressed extent of `ram_bytes` expands into.
fn expanded_len(ram_bytes: u64) -> Result<usize, BtrfsError> {
    usize::try_from(ram_bytes)
        .ok()
        .filter(|&n| n <= MAX_UNCOMPRESSED)
        .ok_or(BtrfsError::BadItem {
            item_type: EXTENT_DATA_KEY,
        })
}

/// The entry in a `DIR_ITEM` payload whose name is `name`, if any.
fn entry_named(payload: &[u8], name: &[u8]) -> Result<Option<Entry>, BtrfsError> {
    for entry in DirItemIter::new(payload, DIR_ITEM_KEY) {
        let entry = entry?;
        if entry.name == name {
            return Ok(Some(Entry {
                target: target_of(&entry)?,
                kind: entry.kind,
            }));
        }
    }
    Ok(None)
}

/// Decode what a directory entry's location key points at.
fn target_of(entry: &DirItem<'_>) -> Result<Target, BtrfsError> {
    match entry.location.item_type {
        INODE_ITEM_KEY => Ok(Target::Inode(entry.location.objectid)),
        ROOT_ITEM_KEY => Ok(Target::Subvolume(entry.location.objectid)),
        _ => Err(BtrfsError::BadItem {
            item_type: DIR_ITEM_KEY,
        }),
    }
}

#[cfg(test)]
mod tests;
