//! The log tree: making one file durable without committing everything.
//!
//! A commit writes every tree the transaction touched. That is the right
//! thing when a transaction has a lot in it and the wrong thing when a
//! program calls `fsync` on one file: the file's bytes are already on the
//! disk, and all that is missing is a durable record of where they are.
//!
//! btrfs keeps that record in a *log tree*, a tree outside the root tree
//! whose address the superblock names in `log_root`. A log commit writes the
//! log's blocks, flushes, and writes a superblock that is the last committed
//! one **plus** that address — so it names the old, whole trees, and the log
//! beside them. Nothing else moves. If the machine survives, the next real
//! commit drops the log ([`WriteVolume::drop_log`]); if it does not, the
//! next mount finds `log_root` set and replays it
//! ([`WriteVolume::replay_log`]), which puts the logged items into the fs
//! tree, commits, and clears the log.
//!
//! # What may be logged
//!
//! One inode's stat data, its file extents and their checksums. Not names:
//! a log that carried half a rename would have to carry the whole of it and
//! the directories either side, which is where Linux's tree-log gets its
//! size and its bugs. A caller that changed the shape of the tree commits
//! instead — [`WriteVolume::log_inode`] says it cannot log, and Linux takes
//! the same way out with `BTRFS_LOG_FORCE_COMMIT`.
//!
//! # Why replay may allocate
//!
//! The data a log names was written in a transaction that never committed,
//! so the committed extent tree does not know those extents and the
//! committed free-space tree calls them free. Replay therefore allocates
//! each logged extent at exactly its address before anything else can — it
//! runs at mount, before the volume is handed to anyone — as Linux's
//! `btrfs_alloc_logged_file_extent` does.

use alloc::vec::Vec;

use ferrix_btrfs::items::{
    CSUM_TREE_OBJECTID, EXTENT_CSUM_KEY, EXTENT_CSUM_OBJECTID, EXTENT_DATA_KEY, ExtentData,
    ExtentDataBody, INODE_ITEM_KEY,
};
use ferrix_btrfs::tree::BtrfsKey;

use crate::bytes::{put_u64, put_u8};
use crate::extent::Backref;
use crate::fs::FS_TREE;
use crate::space::Kind;
use crate::volume::Root;
use crate::{Error, Result, WriteDevice, WriteVolume};

/// The object id every log tree has: Linux's `BTRFS_TREE_LOG_OBJECTID`.
pub const TREE_LOG_OBJECTID: u64 = 0u64.wrapping_sub(6);

/// Superblock field offsets a log commit writes.
const SB_LOG_ROOT: usize = 96;
const SB_LOG_ROOT_TRANSID: usize = 104;
const SB_LOG_ROOT_LEVEL: usize = 200;

impl<D: WriteDevice> WriteVolume<D> {
    /// Whether a log is open, holding items not yet in the fs tree.
    #[must_use]
    pub fn has_log(&self) -> bool {
        self.roots.contains_key(&TREE_LOG_OBJECTID)
    }

    /// Copy everything about inode `ino` into the log: its stat data, its
    /// file extents, and the checksums of the extents they name.
    ///
    /// The caller must have written the file's data first; the log records
    /// where it is, not what it is.
    pub fn log_inode(&mut self, ino: u64) -> Result<()> {
        self.operation(|volume| {
            volume.open_log()?;
            let items = {
                let from = BtrfsKey::new(ino, 0, 0);
                let to = BtrfsKey::new(ino, u8::MAX, u64::MAX);
                volume.range(FS_TREE, &from, &to)?
            };
            if !items.iter().any(|(key, _)| key.item_type == INODE_ITEM_KEY) {
                return Err(Error::NotFound);
            }
            // Anything logged before is replaced, so one inode logged twice
            // in a transaction leaves the later of the two.
            volume.forget_logged(ino)?;
            let mut sums = Vec::new();
            for (key, data) in items {
                if key.item_type == EXTENT_DATA_KEY {
                    sums.extend(logged_extent(&key, &data, volume.sectorsize())?);
                }
                volume.put(TREE_LOG_OBJECTID, key, data)?;
            }
            for (at, len) in sums {
                volume.log_sums(at, len)?;
            }
            Ok(())
        })
    }

    /// Start a log if none is open: an empty leaf owned by the log's id.
    fn open_log(&mut self) -> Result<()> {
        if self.has_log() {
            return Ok(());
        }
        self.ensure_space(64)?;
        let at = self.new_node(TREE_LOG_OBJECTID, 0, crate::node::Body::Leaf(Vec::new()))?;
        let _ = self.roots.insert(
            TREE_LOG_OBJECTID,
            Root {
                bytenr: at,
                level: 0,
                generation: self.transid,
            },
        );
        Ok(())
    }

    /// Drop what the log holds about `ino`, so re-logging replaces it.
    fn forget_logged(&mut self, ino: u64) -> Result<()> {
        let from = BtrfsKey::new(ino, 0, 0);
        let to = BtrfsKey::new(ino, u8::MAX, u64::MAX);
        for (key, _) in self.range(TREE_LOG_OBJECTID, &from, &to)? {
            self.delete(TREE_LOG_OBJECTID, &key)?;
        }
        Ok(())
    }

    /// Copy the checksums of `[at, at + len)` into the log.
    fn log_sums(&mut self, at: u64, len: u64) -> Result<()> {
        let end = at.saturating_add(len);
        let first = BtrfsKey::new(EXTENT_CSUM_OBJECTID, EXTENT_CSUM_KEY, at);
        let before = self
            .prev_item(CSUM_TREE_OBJECTID, &first)?
            .filter(|(key, _)| key.objectid == EXTENT_CSUM_OBJECTID)
            .map_or(first, |(key, _)| key);
        let last = BtrfsKey::new(EXTENT_CSUM_OBJECTID, EXTENT_CSUM_KEY, end.saturating_sub(1));
        for (key, data) in self.range(CSUM_TREE_OBJECTID, &before, &last)? {
            let covered = (data.len() as u64) / 4 * u64::from(self.sectorsize());
            if key.offset.saturating_add(covered) <= at {
                continue;
            }
            self.put(TREE_LOG_OBJECTID, key, data)?;
        }
        Ok(())
    }

    /// Make the log durable: write every block the transaction owns, flush,
    /// and write a superblock that names the last commit's trees and this
    /// log.
    ///
    /// Cheaper than a commit because none of the bookkeeping settles: no
    /// delayed refs run, no block group items, no free-space tree, no root
    /// items. What it costs is that the log must be replayed after a crash.
    pub fn commit_log(&mut self) -> Result<()> {
        self.guarded(|volume| {
            if !volume.has_log() {
                return Ok(());
            }
            let log = volume.root(TREE_LOG_OBJECTID)?;
            volume.write_nodes()?;
            volume.device.flush()?;
            let mut block = volume.superblock.clone();
            let bad = Error::Inconsistent("superblock field out of range");
            put_u64(&mut block, SB_LOG_ROOT, log.bytenr).ok_or(bad)?;
            put_u64(&mut block, SB_LOG_ROOT_TRANSID, volume.transid).ok_or(bad)?;
            put_u8(&mut block, SB_LOG_ROOT_LEVEL, log.level).ok_or(bad)?;
            let block = crate::commit::sealed(block)?;
            volume
                .device
                .write_durable(ferrix_btrfs::superblock::PRIMARY_OFFSET, &block)
        })
    }

    /// Forget the log: its blocks go back to the allocator, and the next
    /// superblock written will not name one. Called by every commit, since a
    /// commit puts everything the log held into the trees themselves.
    pub(crate) fn drop_log(&mut self) -> Result<()> {
        let Some(root) = self.roots.remove(&TREE_LOG_OBJECTID) else {
            return Ok(());
        };
        let mut blocks = Vec::new();
        self.collect_blocks(root, &mut blocks)?;
        for (bytenr, level) in blocks {
            self.dirty.remove(&bytenr);
            let _ = self.clean.remove(&bytenr);
            self.refs.add(
                bytenr,
                u64::from(self.nodesize()),
                Some(level),
                Backref::Tree {
                    root: TREE_LOG_OBJECTID,
                },
                -1,
            )?;
        }
        let _ = self.stale_roots.remove(&TREE_LOG_OBJECTID);
        Ok(())
    }

    /// Every block of the tree at `root`, with its level.
    fn collect_blocks(&mut self, root: Root, out: &mut Vec<(u64, u8)>) -> Result<()> {
        let mut stack = alloc::vec![(root.bytenr, root.level, root.generation)];
        while let Some((at, level, generation)) = stack.pop() {
            self.load(at, level, generation)?;
            out.push((at, level));
            if let crate::node::Body::Internal(ptrs) = &self.node(at)?.body {
                for ptr in ptrs.clone() {
                    stack.push((ptr.blockptr, level.saturating_sub(1), ptr.generation));
                }
            }
        }
        Ok(())
    }

    /// Put everything a log holds into the fs tree, commit, and clear the
    /// log. Run at mount, before anything else may allocate, because the
    /// extents the log names are free as far as the committed trees know.
    pub(crate) fn replay_log(&mut self, bytenr: u64, level: u8, generation: u64) -> Result<()> {
        // Linux stopped writing `log_root_transid` — it is `__unused` there —
        // so a log it wrote says nothing about its generation. The log of a
        // transaction that never committed carries the one after the last
        // commit, which is what its blocks will say.
        let generation = if generation == 0 {
            self.committed.saturating_add(1)
        } else {
            generation
        };
        let root = Root {
            bytenr,
            level,
            generation,
        };
        let _ = self.roots.insert(TREE_LOG_OBJECTID, root);
        let items = self.range(TREE_LOG_OBJECTID, &BtrfsKey::MIN, &BtrfsKey::MAX)?;
        for (key, data) in items {
            match key.item_type {
                EXTENT_CSUM_KEY => self.replay_sums(&key, data)?,
                EXTENT_DATA_KEY => self.replay_extent(&key, data)?,
                _ => self.put(FS_TREE, key, data)?,
            }
        }
        // The log's own blocks are not part of any tree now; they were never
        // in the committed extent tree either, so they are simply free.
        let mut blocks = Vec::new();
        self.collect_blocks(root, &mut blocks)?;
        let _ = self.roots.remove(&TREE_LOG_OBJECTID);
        for (block, _) in blocks {
            let _ = self.dirty.remove(&block);
            let _ = self.clean.remove(&block);
        }
        self.commit()
    }

    /// Replay one file extent: allocate what it names, then record it.
    fn replay_extent(&mut self, key: &BtrfsKey, data: Vec<u8>) -> Result<()> {
        let sector = self.sectorsize();
        let extent = ExtentData::parse_item(key, &data, sector)?;
        let end = extent
            .end(key, sector)
            .ok_or(Error::Inconsistent("logged extent ends nowhere"))?;
        self.drop_extents(key.objectid, key.offset, end)?;
        if let ExtentDataBody::Regular(file) | ExtentDataBody::Prealloc(file) = extent.body
            && file.disk_bytenr != 0
        {
            self.space
                .reserve(file.disk_bytenr, file.disk_num_bytes, Kind::Data)?;
            self.refs.add(
                file.disk_bytenr,
                file.disk_num_bytes,
                None,
                Backref::Data {
                    root: FS_TREE,
                    objectid: key.objectid,
                    offset: key.offset.wrapping_sub(file.offset),
                },
                1,
            )?;
        }
        self.put(FS_TREE, *key, data)
    }

    /// Replay a run of checksums, leaving whatever the tree already holds.
    fn replay_sums(&mut self, key: &BtrfsKey, data: Vec<u8>) -> Result<()> {
        let covered = (data.len() as u64) / 4 * u64::from(self.sectorsize());
        crate::csum::delete_range(self, key.offset, covered)?;
        self.put(CSUM_TREE_OBJECTID, *key, data)
    }
}

/// The disk range a file extent's checksums cover, if it has any.
fn logged_extent(key: &BtrfsKey, data: &[u8], sectorsize: u32) -> Result<Option<(u64, u64)>> {
    let extent = ExtentData::parse_item(key, data, sectorsize)?;
    let ExtentDataBody::Regular(file) = extent.body else {
        return Ok(None);
    };
    if file.disk_bytenr == 0 {
        return Ok(None);
    }
    let (at, len) = if extent.is_uncompressed() {
        (file.disk_bytenr.saturating_add(file.offset), file.num_bytes)
    } else {
        (file.disk_bytenr, file.disk_num_bytes)
    };
    Ok(Some((at, len)))
}
