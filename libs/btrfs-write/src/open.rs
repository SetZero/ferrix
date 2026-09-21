//! Opening a volume for writing: everything the writer needs in memory.
//!
//! The reader bootstraps from the superblock to the default subvolume. The
//! writer needs more, and needs it to be consistent before it may change a
//! byte: the chunk layout with every stripe, the root of every tree, each
//! block group's usage, and what the free-space tree says is free in each.
//! Every one of those is checked against the others as it is loaded — a
//! block group with no chunk, a free-space count that disagrees with its
//! items — because a writer that trusts a wrong free map hands out space
//! something already uses.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;

use ferrix_btrfs::chunk::{ChunkItem, FIRST_CHUNK_TREE_OBJECTID};
use ferrix_btrfs::items::{
    CHUNK_ITEM_KEY, CHUNK_TREE_OBJECTID, DEV_ITEM_KEY, DEV_ITEMS_OBJECTID, DevItem,
    EXTENT_TREE_OBJECTID, FIRST_FREE_OBJECTID, LAST_FREE_OBJECTID, ROOT_ITEM_KEY,
    ROOT_TREE_OBJECTID, RootItem,
};
use ferrix_btrfs::superblock::{PRIMARY_OFFSET, SUPERBLOCK_SIZE, Superblock};
use ferrix_btrfs::tree::{BtrfsKey, NodeHeader};
use ferrix_btrfs::volume::ReadKind;

use crate::bytes::{get_u32, get_u64};
use crate::chunks::{Chunk, Chunks};
use crate::commit::FREE_SPACE_TREE_OBJECTID;
use crate::extent::{
    BLOCK_GROUP_ITEM_KEY, FREE_SPACE_BITMAP_KEY, FREE_SPACE_EXTENT_KEY, FREE_SPACE_INFO_KEY,
};
use crate::ranges::RangeSet;
use crate::refs::DelayedRefs;
use crate::space::{BlockGroup, Space};
use crate::volume::{Geometry, Root};
use crate::{Error, Result, Unsupported, WriteDevice, WriteVolume};

/// `compat_ro` bit: the free-space tree exists.
const FREE_SPACE_TREE: u64 = 1 << 0;
/// `compat_ro` bit: the free-space tree is complete and may be trusted.
const FREE_SPACE_TREE_VALID: u64 = 1 << 1;
/// `compat_ro` bit: fs-verity items may exist; they are file items this
/// writer never touches, so a volume with them stays writable.
const VERITY: u64 = 1 << 2;
/// The quota tree's id.
const QUOTA_TREE_OBJECTID: u64 = 8;
/// Object id of log tree roots in the root tree.
const TREE_LOG_OBJECTID: u64 = 0u64.wrapping_sub(6);
/// Object id of orphan items in the root tree: subvolumes being deleted.
const ORPHAN_OBJECTID: u64 = 0u64.wrapping_sub(5);
/// `ROOT_REF` and `ROOT_BACKREF`: links between subvolumes.
const ROOT_BACKREF_KEY: u8 = 144;
const ROOT_REF_KEY: u8 = 156;
/// Free-space info flag: the group is recorded as bitmaps.
const USING_BITMAPS: u32 = 1 << 0;

impl<D: WriteDevice> WriteVolume<D> {
    /// Open the volume on `device` for writing.
    ///
    /// [`Error::Unsupported`] names the reason a volume this writer does not
    /// maintain is refused; such a volume can still be read with
    /// `ferrix-btrfs`.
    pub fn open(device: D) -> Result<Self> {
        let mut volume = Self::open_committed(device)?;
        // Files unlinked while open when the volume was last in use: nothing
        // can hold them open now, so they go, as Linux's orphan cleanup at
        // mount. The deletion is part of the first transaction.
        for ino in volume.orphans()? {
            volume.evict(ino)?;
        }
        Ok(volume)
    }

    /// Load the committed state and nothing more: no orphan cleanup, so the
    /// transaction starts empty. What a checker wants.
    pub(crate) fn open_committed(mut device: D) -> Result<Self> {
        let mut block = vec![0u8; SUPERBLOCK_SIZE];
        device.read_at(PRIMARY_OFFSET, &mut block, ReadKind::Metadata)?;
        let sb = Superblock::parse_at(&block, PRIMARY_OFFSET)?;
        check_writable(&sb)?;
        let dev = DevItem::parse(sb.dev_item())?;
        let mut chunks = Chunks::default();
        for entry in sb.sys_chunk_array() {
            let (key, item) = entry?;
            chunks.insert(Chunk::from_item(key.offset, &item)?)?;
        }
        let mut volume = WriteVolume {
            device,
            geometry: Geometry {
                nodesize: sb.nodesize(),
                sectorsize: sb.sectorsize(),
                fsid: sb.fsid(),
                chunk_tree_uuid: [0; 16],
                devid: dev.devid,
                dev_uuid: dev.uuid,
                device_size: dev.total_bytes,
            },
            chunks,
            superblock: block.clone(),
            committed: sb.generation(),
            transid: sb.generation().saturating_add(1),
            dirty: BTreeMap::new(),
            clean: BTreeMap::new(),
            roots: BTreeMap::new(),
            stale_roots: BTreeSet::new(),
            refs: DelayedRefs::default(),
            space: Space::default(),
            chunks_changed: false,
            growing: false,
            aborted: false,
            edits: 0,
        };
        let _ = volume.roots.insert(
            CHUNK_TREE_OBJECTID,
            Root {
                bytenr: sb.chunk_root(),
                level: sb.chunk_root_level(),
                generation: sb.chunk_root_generation(),
            },
        );
        let _ = volume.roots.insert(
            ROOT_TREE_OBJECTID,
            Root {
                bytenr: sb.root(),
                level: sb.root_level(),
                generation: sb.generation(),
            },
        );
        volume.geometry.chunk_tree_uuid = volume.read_chunk_tree_uuid(sb.chunk_root())?;
        volume.load_chunks()?;
        volume.load_roots()?;
        volume.load_block_groups()?;
        Ok(volume)
    }

    /// Throw the running transaction away and reload the committed state.
    ///
    /// Nothing the transaction did reached the disk in a form the superblock
    /// names — nodes it wrote went to space the committed trees do not use —
    /// so the last commit is intact and reopening from it is complete.
    pub fn abort(self) -> Result<Self> {
        Self::open(self.device)
    }

    fn read_chunk_tree_uuid(&mut self, chunk_root: u64) -> Result<[u8; 16]> {
        let size = self.geometry.nodesize as usize;
        let mut buf = vec![0u8; size];
        let copies = self
            .chunks
            .copies(chunk_root, u64::from(self.geometry.nodesize))?;
        let first = copies
            .first()
            .copied()
            .ok_or(Error::Inconsistent("chunk root has no copy"))?;
        self.device.read_at(first, &mut buf, ReadKind::Metadata)?;
        Ok(NodeHeader::parse(&buf)?.chunk_tree_uuid)
    }

    /// Every chunk in the chunk tree, with all its stripes.
    fn load_chunks(&mut self) -> Result<()> {
        let from = BtrfsKey::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, 0);
        let to = BtrfsKey::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, u64::MAX);
        for (key, data) in self.range(CHUNK_TREE_OBJECTID, &from, &to)? {
            let item = ChunkItem::parse(&data)?;
            item.check_sectorsize(key.offset, self.geometry.sectorsize)?;
            self.chunks.insert(Chunk::from_item(key.offset, &item)?)?;
        }
        let dev_key = BtrfsKey::new(DEV_ITEMS_OBJECTID, DEV_ITEM_KEY, self.geometry.devid);
        let dev = self
            .get(CHUNK_TREE_OBJECTID, &dev_key)?
            .ok_or(Error::Inconsistent("device has no DEV_ITEM"))?;
        let dev = DevItem::parse(&dev)?;
        if dev.uuid != self.geometry.dev_uuid || dev.total_bytes != self.geometry.device_size {
            return Err(Error::Inconsistent(
                "DEV_ITEM disagrees with the superblock",
            ));
        }
        Ok(())
    }

    /// The root of every tree the root tree lists, refusing a volume with
    /// anything this writer cannot keep consistent.
    fn load_roots(&mut self) -> Result<()> {
        let items = self.range(ROOT_TREE_OBJECTID, &BtrfsKey::MIN, &BtrfsKey::MAX)?;
        for (key, data) in items {
            let id = key.objectid;
            match key.item_type {
                ROOT_ITEM_KEY if id == QUOTA_TREE_OBJECTID => {
                    return Err(Error::Unsupported(Unsupported::Quotas));
                }
                ROOT_ITEM_KEY if id == TREE_LOG_OBJECTID => {
                    return Err(Error::Unsupported(Unsupported::Log));
                }
                ROOT_ITEM_KEY
                    if (FIRST_FREE_OBJECTID..=LAST_FREE_OBJECTID).contains(&id)
                        || key.offset != 0 =>
                {
                    return Err(Error::Unsupported(Unsupported::Subvolumes));
                }
                ROOT_ITEM_KEY => {
                    let item = RootItem::parse(&data)?;
                    let root = Root {
                        bytenr: item.bytenr,
                        level: item.level,
                        generation: item.generation,
                    };
                    let _ = self.roots.insert(id, root);
                }
                ROOT_REF_KEY | ROOT_BACKREF_KEY => {
                    return Err(Error::Unsupported(Unsupported::Subvolumes));
                }
                _ if id == ORPHAN_OBJECTID => {
                    return Err(Error::Unsupported(Unsupported::Subvolumes));
                }
                _ => {}
            }
        }
        for tree in [
            EXTENT_TREE_OBJECTID,
            FREE_SPACE_TREE_OBJECTID,
            ferrix_btrfs::items::CSUM_TREE_OBJECTID,
            ferrix_btrfs::items::DEV_TREE_OBJECTID,
        ] {
            let _ = self.root(tree)?;
        }
        Ok(())
    }

    /// A block group for every chunk, with its usage and free space.
    fn load_block_groups(&mut self) -> Result<()> {
        let chunks: Vec<(u64, u64, u64)> = self
            .chunks
            .iter()
            .map(|chunk| (chunk.logical, chunk.length, chunk.type_bits))
            .collect();
        for (start, length, type_bits) in chunks {
            let key = BtrfsKey::new(start, BLOCK_GROUP_ITEM_KEY, length);
            let item = self
                .get(EXTENT_TREE_OBJECTID, &key)?
                .ok_or(Error::Inconsistent("chunk without a block group item"))?;
            let bad = Error::Inconsistent("malformed block group item");
            let used = get_u64(&item, 0).ok_or(bad)?;
            let flags = get_u64(&item, 16).ok_or(bad)?;
            if flags != type_bits || used > length {
                return Err(bad);
            }
            let (free, bitmaps) = self.load_free_space(start, length)?;
            self.space.insert(BlockGroup {
                start,
                length,
                flags,
                used,
                item_dirty: false,
                free: free.clone(),
                pinned: RangeSet::new(),
                on_disk: free,
                bitmaps,
            })?;
        }
        Ok(())
    }

    /// What the free-space tree records as free in the group at `start`, and
    /// whether it records it as bitmaps.
    fn load_free_space(&mut self, start: u64, length: u64) -> Result<(RangeSet, bool)> {
        let bad = Error::Inconsistent("free-space tree disagrees with itself");
        let tree = FREE_SPACE_TREE_OBJECTID;
        let info = self
            .get(tree, &BtrfsKey::new(start, FREE_SPACE_INFO_KEY, length))?
            .ok_or(Error::Unsupported(Unsupported::NoFreeSpaceTree))?;
        let count = get_u32(&info, 0).ok_or(bad)?;
        let bitmaps = get_u32(&info, 4).ok_or(bad)? & USING_BITMAPS != 0;
        let end = start.checked_add(length).ok_or(bad)?;
        let from = BtrfsKey::new(start, FREE_SPACE_EXTENT_KEY, 0);
        let to = BtrfsKey::new(end.saturating_sub(1), FREE_SPACE_BITMAP_KEY, u64::MAX);
        let mut free = RangeSet::new();
        let mut extents = 0u32;
        for (key, data) in self.range(tree, &from, &to)? {
            let run_end = key.objectid.checked_add(key.offset).ok_or(bad)?;
            if run_end > end {
                return Err(bad);
            }
            match key.item_type {
                FREE_SPACE_EXTENT_KEY if !bitmaps => {
                    if !free.insert(key.objectid, key.offset) {
                        return Err(bad);
                    }
                    extents = extents.saturating_add(1);
                }
                FREE_SPACE_BITMAP_KEY if bitmaps => self.add_bitmap(&mut free, &key, &data)?,
                _ => return Err(bad),
            }
        }
        // A bitmap group's count is of the runs its bits describe.
        let runs = if bitmaps {
            u32::try_from(free.runs()).map_err(|_| bad)?
        } else {
            extents
        };
        if runs != count {
            return Err(bad);
        }
        Ok((free, bitmaps))
    }

    /// Add the free sectors a `FREE_SPACE_BITMAP` item marks.
    fn add_bitmap(&self, free: &mut RangeSet, key: &BtrfsKey, data: &[u8]) -> Result<()> {
        let bad = Error::Inconsistent("malformed free-space bitmap");
        let sector = u64::from(self.geometry.sectorsize);
        let bits = key.offset / sector;
        if bits.div_ceil(8) != data.len() as u64 {
            return Err(bad);
        }
        for bit in 0..bits {
            let byte = data
                .get(usize::try_from(bit / 8).map_err(|_| bad)?)
                .copied()
                .ok_or(bad)?;
            if byte & (1 << (bit % 8)) != 0 {
                let at = key
                    .objectid
                    .checked_add(bit.saturating_mul(sector))
                    .ok_or(bad)?;
                if !free.insert(at, sector) {
                    return Err(bad);
                }
            }
        }
        Ok(())
    }
}

/// Refuse a volume this writer would damage.
fn check_writable(sb: &Superblock<'_>) -> Result<()> {
    use ferrix_btrfs::superblock::IncompatFlags;
    let incompat = sb.incompat_flags();
    if incompat.unknown() != 0 {
        return Err(Error::Volume(ferrix_btrfs::BtrfsError::UnsupportedFeature(
            incompat.unknown(),
        )));
    }
    if sb.num_devices() != 1 {
        return Err(Error::Unsupported(Unsupported::MultipleDevices));
    }
    if !incompat.skinny_metadata() {
        return Err(Error::Unsupported(Unsupported::NotSkinny));
    }
    if !incompat.no_holes() {
        return Err(Error::Unsupported(Unsupported::NoHoles));
    }
    if incompat.contains(IncompatFlags::MIXED_GROUPS) {
        return Err(Error::Unsupported(Unsupported::MixedGroups));
    }
    if incompat.contains(IncompatFlags::RAID56) || incompat.contains(IncompatFlags::RAID1C34) {
        return Err(Error::Unsupported(Unsupported::Profile));
    }
    let compat_ro = sb.compat_ro_flags();
    let fst = FREE_SPACE_TREE | FREE_SPACE_TREE_VALID;
    if compat_ro & fst != fst {
        return Err(Error::Unsupported(Unsupported::NoFreeSpaceTree));
    }
    let other = compat_ro & !(fst | VERITY);
    if other != 0 {
        return Err(Error::Unsupported(Unsupported::CompatRo(other)));
    }
    if sb.log_root() != 0 {
        return Err(Error::Unsupported(Unsupported::Log));
    }
    Ok(())
}
