//! Allocating a chunk: turning free device space into a new block group.
//!
//! `mkfs.btrfs` makes one small chunk of each kind — on a default volume an
//! 8 MiB data chunk — and leaves the rest of the device unallocated, so
//! writing anything sizeable means making chunks. A chunk touches five
//! places, and all five must agree for `btrfs check` to accept the volume:
//!
//! 1. a `CHUNK_ITEM` in the chunk tree, and for a system chunk a copy in the
//!    superblock's system chunk array;
//! 2. one `DEV_EXTENT` per stripe in the device tree, saying which bytes of
//!    the device the stripe occupies;
//! 3. the device's `DEV_ITEM`, in the chunk tree and embedded in the
//!    superblock, whose `bytes_used` grows by every stripe;
//! 4. a `BLOCK_GROUP_ITEM` in the extent tree;
//! 5. a `FREE_SPACE_INFO` and one `FREE_SPACE_EXTENT` covering it all.
//!
//! Recording those allocates tree blocks, and the chunk being made may be the
//! only place with room for them — it is being made because a kind ran out.
//! So the chunk joins the in-memory layout and allocator first, and the items
//! are written after, allocating from it like any other group.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_btrfs::chunk::{BLOCK_GROUP_PROFILE_MASK, Stripe};
use ferrix_btrfs::items::{
    CHUNK_ITEM_KEY, CHUNK_TREE_OBJECTID, DEV_ITEM_KEY, DEV_ITEMS_OBJECTID, DEV_TREE_OBJECTID,
    EXTENT_TREE_OBJECTID,
};
use ferrix_btrfs::tree::BtrfsKey;

use crate::bytes::{key_bytes, put, put_u32, put_u64};
use crate::chunks::Chunk;
use crate::commit::FREE_SPACE_TREE_OBJECTID;
use crate::extent::{
    BLOCK_GROUP_ITEM_KEY, DEV_EXTENT_KEY, FREE_SPACE_EXTENT_KEY, FREE_SPACE_INFO_KEY,
};
use crate::ranges::RangeSet;
use crate::space::{BlockGroup, Kind};
use crate::{Error, Result, WriteDevice, WriteVolume};

/// Device space below this is never allocated: it holds the boot area and
/// the primary superblock. Linux's `BTRFS_DEVICE_RANGE_RESERVED`.
const DEVICE_RESERVED: u64 = 1024 * 1024;
/// Chunk sizes are multiples of this.
const CHUNK_ALIGN: u64 = 1024 * 1024;
/// The object id chunk items and device extents name as the chunk tree's
/// first chunk.
const FIRST_CHUNK_TREE_OBJECTID: u64 = 256;

impl<D: WriteDevice> WriteVolume<D> {
    /// The largest chunk of `kind` Linux would make on this device: 1 GiB of
    /// data, 256 MiB of metadata, 32 MiB of system, never more than a tenth
    /// of the device.
    fn chunk_size(&self, kind: Kind) -> u64 {
        let most = match kind {
            Kind::Data => 1024 * 1024 * 1024,
            Kind::Metadata => 256 * 1024 * 1024,
            Kind::System => 32 * 1024 * 1024,
        };
        let tenth = self.geometry.device_size / 10;
        let size = most.min(tenth);
        (size - size % CHUNK_ALIGN).max(CHUNK_ALIGN)
    }

    /// The profile bits new chunks of `kind` get: whatever the volume already
    /// uses for that kind, so a DUP-metadata volume stays DUP.
    fn profile_for(&self, kind: Kind) -> u64 {
        self.chunks
            .iter()
            .find(|chunk| chunk.kind() == kind.bits())
            .map_or(0, |chunk| chunk.type_bits & BLOCK_GROUP_PROFILE_MASK)
    }

    /// The unallocated stretches of the device.
    fn device_holes(&self) -> RangeSet {
        let mut holes = RangeSet::new();
        let _ = holes.insert(
            DEVICE_RESERVED,
            self.geometry.device_size.saturating_sub(DEVICE_RESERVED),
        );
        for chunk in self.chunks.iter() {
            for stripe in &chunk.stripes {
                let _ = holes.remove(stripe.offset, chunk.length);
            }
        }
        holes
    }

    /// Allocate a chunk of `kind`, as large as the device allows up to
    /// [`Self::chunk_size`], and record it everywhere.
    pub(crate) fn allocate_chunk(&mut self, kind: Kind) -> Result<()> {
        let profile = self.profile_for(kind);
        let copies: u64 = if profile == 0 { 1 } else { 2 };
        let mut holes = self.device_holes();
        let mut size = self.chunk_size(kind);
        let mut stripes = Vec::new();
        while stripes.len() < copies as usize {
            let largest = holes
                .iter()
                .map(|(_, len)| len - len % CHUNK_ALIGN)
                .max()
                .unwrap_or(0);
            size = size.min(largest);
            if size == 0 {
                return Err(Error::NoSpace);
            }
            let (at, _) = holes
                .first_prefix(size, size, CHUNK_ALIGN, 0)
                .ok_or(Error::NoSpace)?;
            let _ = holes.remove(at, size);
            stripes.push(at);
        }
        let chunk = Chunk {
            logical: self.chunks.next_logical().next_multiple_of(CHUNK_ALIGN),
            length: size,
            type_bits: kind.bits() | profile,
            stripes: stripes
                .iter()
                .map(|&offset| Stripe {
                    devid: self.geometry.devid,
                    offset,
                    dev_uuid: self.geometry.dev_uuid,
                })
                .collect(),
        };
        let mut free = RangeSet::new();
        let _ = free.insert(chunk.logical, chunk.length);
        self.space.insert(BlockGroup {
            start: chunk.logical,
            length: chunk.length,
            flags: chunk.type_bits,
            used: 0,
            item_dirty: false,
            free: free.clone(),
            pinned: RangeSet::new(),
            on_disk: free,
            bitmaps: false,
        })?;
        self.chunks.insert(chunk.clone())?;
        self.chunks_changed = true;
        self.record_chunk(&chunk)
    }

    /// Write the items describing a new chunk into the four trees.
    fn record_chunk(&mut self, chunk: &Chunk) -> Result<()> {
        let bad = Error::Inconsistent("chunk item does not encode");
        let item = chunk.item_bytes(self.sectorsize()).ok_or(bad)?;
        self.insert(
            CHUNK_TREE_OBJECTID,
            BtrfsKey::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, chunk.logical),
            item,
        )?;
        for stripe in &chunk.stripes {
            let mut extent = vec![0u8; 48];
            put_u64(&mut extent, 0, CHUNK_TREE_OBJECTID).ok_or(bad)?;
            put_u64(&mut extent, 8, FIRST_CHUNK_TREE_OBJECTID).ok_or(bad)?;
            put_u64(&mut extent, 16, chunk.logical).ok_or(bad)?;
            put_u64(&mut extent, 24, chunk.length).ok_or(bad)?;
            put(&mut extent, 32, &self.geometry.chunk_tree_uuid).ok_or(bad)?;
            let key = BtrfsKey::new(stripe.devid, DEV_EXTENT_KEY, stripe.offset);
            self.insert(DEV_TREE_OBJECTID, key, extent)?;
        }
        let dev_key = BtrfsKey::new(DEV_ITEMS_OBJECTID, DEV_ITEM_KEY, self.geometry.devid);
        let mut dev_item = self
            .get(CHUNK_TREE_OBJECTID, &dev_key)?
            .ok_or(Error::Inconsistent("device has no DEV_ITEM"))?;
        put_u64(&mut dev_item, 16, self.device_bytes_used()).ok_or(bad)?;
        self.update(CHUNK_TREE_OBJECTID, dev_key, dev_item)?;
        let mut group = vec![0u8; 24];
        put_u64(&mut group, 0, 0).ok_or(bad)?;
        put_u64(&mut group, 8, FIRST_CHUNK_TREE_OBJECTID).ok_or(bad)?;
        put_u64(&mut group, 16, chunk.type_bits).ok_or(bad)?;
        let key = BtrfsKey::new(chunk.logical, BLOCK_GROUP_ITEM_KEY, chunk.length);
        self.insert(EXTENT_TREE_OBJECTID, key, group)?;
        let mut info = vec![0u8; 8];
        put_u32(&mut info, 0, 1).ok_or(bad)?;
        let tree = FREE_SPACE_TREE_OBJECTID;
        self.insert(
            tree,
            BtrfsKey::new(chunk.logical, FREE_SPACE_INFO_KEY, chunk.length),
            info,
        )?;
        self.insert(
            tree,
            BtrfsKey::new(chunk.logical, FREE_SPACE_EXTENT_KEY, chunk.length),
            Vec::new(),
        )
    }

    /// The superblock's system chunk array: the key and item of every system
    /// chunk, back to back.
    pub(crate) fn system_chunk_array(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for chunk in self
            .chunks
            .iter()
            .filter(|chunk| chunk.kind() == Kind::System.bits())
        {
            let key = BtrfsKey::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, chunk.logical);
            out.extend_from_slice(&key_bytes(&key));
            out.extend(
                chunk
                    .item_bytes(self.sectorsize())
                    .ok_or(Error::Inconsistent("chunk item does not encode"))?,
            );
        }
        if out.len() > ferrix_btrfs::superblock::SYS_CHUNK_ARRAY_SIZE {
            return Err(Error::NoSpace);
        }
        Ok(out)
    }
}
