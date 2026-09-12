//! Reading a volume: from the superblock on a device to any item in any tree.
//!
//! Everything below [`Volume`] in this crate is a function of bytes somebody
//! else read. This is the layer that does the reading, through a [`Device`] the
//! caller implements, and it performs the bootstrap the crate documentation
//! lays out: superblock, system chunk array, chunk tree, root tree, and from
//! the root tree the default subvolume's fs tree.
//!
//! # One node buffer, and no path
//!
//! A B-tree reader normally keeps a path — one buffer per level — so it can
//! step to the next leaf by climbing back up. btrfs trees are up to eight
//! levels of nodes up to 64 KiB each, which is half a megabyte of buffers that
//! would have to exist before the heap does. So this reader holds exactly one
//! node at a time, in a buffer the caller supplies, and instead of climbing it
//! descends again from the root.
//!
//! What makes that work is remembering, on the way down, the smallest key
//! known to lie *after* the leaf reached: the key of the next sibling pointer
//! at the deepest level that has one. When a leaf is exhausted, iteration
//! continues by seeking that key. With a block cache under the [`Device`] the
//! repeated descent costs a few cache hits per leaf.
//!
//! # Why iteration terminates on a hostile tree
//!
//! A seek for `key` picks, at each level, the last pointer whose key is
//! `<= key`, so the sibling after it has a key strictly greater than `key`.
//! Every continuation key is therefore strictly greater than the one before,
//! and each is a key stored in some reachable node. There are finitely many,
//! so the walk ends. Descent itself cannot cycle: a child must sit exactly one
//! level below its parent, checked on every read, so no path is longer than
//! the root's level. And a leaf only yields items below its continuation key,
//! so a node that lies about its range cannot make an item appear twice.
//!
//! # What is checked on every node
//!
//! [`Node::parse`] checks the checksum, the self-recorded address and the item
//! layout. Here, on top: the level the parent promised, the generation the
//! parent recorded for the child — Linux's "parent transid verify failed" —
//! and the filesystem id. Any of those failing means the pointer led somewhere
//! other than the block it was written for.

use core::ops::ControlFlow;

use crate::chunk::{ChunkItem, ChunkMap, ChunkMapEntry, FIRST_CHUNK_TREE_OBJECTID};
use crate::items::{CHUNK_ITEM_KEY, FS_TREE_OBJECTID, ROOT_ITEM_KEY, RootItem};
use crate::superblock::{IncompatFlags, PRIMARY_OFFSET, SUPERBLOCK_SIZE, Superblock};
use crate::tree::{BtrfsKey, Item, Node, NodeHeader};
use crate::{BtrfsError, truncated};

/// The highest level a tree node may have. btrfs trees have at most eight
/// levels, numbered from zero at the leaves.
pub const MAX_LEVEL: u8 = 7;

/// Byte-addressed access to the one device a volume lives on.
///
/// The reader never asks for anything but whole nodes and whole extents, so an
/// implementation over a block cache can serve every call from cached sectors.
pub trait Device {
    /// Fill `buf` with the device's bytes starting at `physical`.
    ///
    /// A short device or a failed read is an error, typically
    /// [`BtrfsError::DeviceRead`]; a partially filled buffer must never be
    /// reported as success.
    fn read_at(&mut self, physical: u64, buf: &mut [u8]) -> Result<(), BtrfsError>;
}

/// Where a tree starts, and what its top node must say about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeRoot {
    /// *Logical* address of the top node.
    pub bytenr: u64,
    /// Level of the top node; zero for a tree that is a single leaf.
    pub level: u8,
    /// Generation the top node must carry.
    pub generation: u64,
}

/// A leaf reached by a seek, and where iteration stands within it.
#[derive(Debug, Clone, Copy)]
pub struct Leaf<'b> {
    /// The leaf, borrowing the caller's node buffer.
    pub node: Node<'b>,
    /// Slot of the first item `>=` the key sought; `nritems` if none is.
    pub slot: u32,
    /// The smallest key known to lie beyond this leaf, where iteration
    /// continues. `None` when the leaf is the last in the tree.
    pub next: Option<BtrfsKey>,
    /// The first key of the deepest subtree entered through a pointer other
    /// than the leftmost. Everything before this leaf sorts below it, which is
    /// how [`Volume::last_at_or_before`] finds the previous leaf.
    boundary: Option<BtrfsKey>,
}

impl<'b> Leaf<'b> {
    /// The items from [`Leaf::slot`] up to the continuation key.
    pub fn items(&self) -> impl Iterator<Item = Item<'b>> + use<'b> {
        let next = self.next;
        self.node
            .items()
            .skip(self.slot as usize)
            .take_while(move |item| next.is_none_or(|limit| item.key < limit))
    }
}

/// A mounted view of a single-device volume, read-only.
///
/// Holds nothing but the chunk map and a few superblock fields, so it is cheap
/// to keep for the life of a mount. Every read takes the device and a node
/// buffer of at least [`Volume::nodesize`] bytes from the caller.
#[derive(Debug)]
pub struct Volume<'m> {
    chunks: ChunkMap<'m>,
    fsid: [u8; 16],
    generation: u64,
    nodesize: u32,
    sectorsize: u32,
    incompat: IncompatFlags,
    chunk_tree: TreeRoot,
    root_tree: TreeRoot,
    fs_tree: TreeRoot,
    root_dir: u64,
}

impl<'m> Volume<'m> {
    /// Open the volume on `device`.
    ///
    /// `chunks` is the storage for the chunk map, and bounds how many chunks
    /// the volume may have; `node` is the buffer every node is read into. Only
    /// the primary superblock is read: the mirrors are for recovery, and a
    /// reader that silently fell back to one would mount a filesystem Linux
    /// refuses.
    pub fn open<D: Device>(
        device: &mut D,
        chunks: &'m mut [ChunkMapEntry],
        node: &mut [u8],
    ) -> Result<Self, BtrfsError> {
        let mut block = [0u8; SUPERBLOCK_SIZE];
        device.read_at(PRIMARY_OFFSET, &mut block)?;
        let sb = Superblock::parse_at(&block, PRIMARY_OFFSET)?;
        check_mountable(&sb)?;

        let mut map = ChunkMap::new(chunks);
        if map.load_sys_chunk_array(sb.sys_chunk_array_bytes())? == 0 {
            // Without a system chunk nothing maps the chunk tree, so say which
            // address was unreachable rather than failing on it a step later.
            return Err(BtrfsError::NotMapped(sb.chunk_root()));
        }
        let mut volume = Volume {
            chunks: map,
            fsid: sb.fsid(),
            generation: sb.generation(),
            nodesize: sb.nodesize(),
            sectorsize: sb.sectorsize(),
            incompat: sb.incompat_flags(),
            chunk_tree: TreeRoot {
                bytenr: sb.chunk_root(),
                level: sb.chunk_root_level(),
                generation: sb.chunk_root_generation(),
            },
            root_tree: TreeRoot {
                bytenr: sb.root(),
                level: sb.root_level(),
                generation: sb.generation(),
            },
            // Filled in below, once the root tree is reachable.
            fs_tree: TreeRoot {
                bytenr: 0,
                level: 0,
                generation: 0,
            },
            root_dir: 0,
        };
        volume.load_chunk_tree(device, node)?;
        let root = volume.find_root(device, FS_TREE_OBJECTID, node)?;
        volume.fs_tree = TreeRoot {
            bytenr: root.bytenr,
            level: root.level,
            generation: root.generation,
        };
        volume.root_dir = root.root_dirid;
        Ok(volume)
    }

    /// Size of every tree node, and the least a node buffer must hold.
    #[must_use]
    pub const fn nodesize(&self) -> u32 {
        self.nodesize
    }

    /// Size of a data sector.
    #[must_use]
    pub const fn sectorsize(&self) -> u32 {
        self.sectorsize
    }

    /// The superblock's generation: the last committed transaction.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The filesystem id every node must carry.
    #[must_use]
    pub const fn fsid(&self) -> [u8; 16] {
        self.fsid
    }

    /// Incompatible features the volume uses.
    #[must_use]
    pub const fn incompat(&self) -> IncompatFlags {
        self.incompat
    }

    /// The default subvolume's fs tree.
    #[must_use]
    pub const fn fs_tree(&self) -> TreeRoot {
        self.fs_tree
    }

    /// Inode number of the default subvolume's root directory.
    #[must_use]
    pub const fn root_dir(&self) -> u64 {
        self.root_dir
    }

    /// The logical-to-physical map, as loaded from the chunk tree.
    #[must_use]
    pub const fn chunks(&self) -> &ChunkMap<'m> {
        &self.chunks
    }

    /// Read `buf.len()` bytes of data starting at logical address `logical`.
    ///
    /// The range must lie inside one chunk. An extent never spans chunks, so a
    /// range that does was not produced by a well-formed item.
    pub fn read_logical<D: Device>(
        &self,
        device: &mut D,
        logical: u64,
        buf: &mut [u8],
    ) -> Result<(), BtrfsError> {
        let (_devid, physical) = self.chunks.map(logical)?;
        let span = self.chunks.contiguous_len(logical).unwrap_or(0);
        if span < buf.len() as u64 {
            return Err(BtrfsError::NotMapped(logical.saturating_add(span)));
        }
        device.read_at(physical, buf)
    }

    /// Descend `root` to the leaf that holds `key` or would.
    pub fn seek<'b, D: Device>(
        &self,
        device: &mut D,
        root: TreeRoot,
        key: &BtrfsKey,
        node: &'b mut [u8],
    ) -> Result<Leaf<'b>, BtrfsError> {
        if root.level > MAX_LEVEL {
            return Err(BtrfsError::BadTree {
                logical: root.bytenr,
            });
        }
        let size = self.nodesize as usize;
        let mut at = root;
        let mut next = None;
        let mut boundary = None;
        while at.level > 0 {
            let found = node.len();
            let block = node.get_mut(..size).ok_or_else(|| truncated(size, found))?;
            self.read_node(device, at, block)?;
            let parent = Node::parse(block, at.bytenr)?;
            let slot = parent.search_slot(key).unwrap_or(0);
            if let Some(sibling) = slot.checked_add(1).and_then(|s| parent.key(s)) {
                next = Some(sibling);
            }
            if slot > 0 {
                boundary = parent.key(slot);
            }
            let child = parent
                .key_ptr(slot)
                .ok_or(BtrfsError::BadTree { logical: at.bytenr })?;
            at = TreeRoot {
                bytenr: child.blockptr,
                level: at.level.saturating_sub(1),
                generation: child.generation,
            };
        }
        let found = node.len();
        let block = node.get_mut(..size).ok_or_else(|| truncated(size, found))?;
        self.read_node(device, at, block)?;
        let block: &'b [u8] = block;
        let leaf = Node::parse(block, at.bytenr)?;
        let (Ok(slot) | Err(slot)) = leaf.search(key);
        Ok(Leaf {
            node: leaf,
            slot,
            next,
            boundary,
        })
    }

    /// Visit every item of `root` from the first one `>= key`, in key order,
    /// until `visit` breaks or the tree ends.
    ///
    /// `visit` gets the device back so it can read data an item points at, and
    /// an item that borrows the node buffer — anything it wants to keep it
    /// copies out. Returns the break value, or `None` if the tree ran out.
    pub fn walk<D: Device, B>(
        &self,
        device: &mut D,
        root: TreeRoot,
        key: BtrfsKey,
        node: &mut [u8],
        mut visit: impl FnMut(&mut D, Item<'_>) -> Result<ControlFlow<B>, BtrfsError>,
    ) -> Result<Option<B>, BtrfsError> {
        let mut key = key;
        loop {
            let leaf = self.seek(device, root, &key, node)?;
            for item in leaf.items() {
                if let ControlFlow::Break(value) = visit(device, item)? {
                    return Ok(Some(value));
                }
            }
            match leaf.next {
                Some(next) => key = next,
                None => return Ok(None),
            }
        }
    }

    /// The key of the last item in `root` that is `<= key`, if there is one.
    ///
    /// This is how a read finds the extent covering an offset: the extent item
    /// for a byte is the last one starting at or before it.
    pub fn last_at_or_before<D: Device>(
        &self,
        device: &mut D,
        root: TreeRoot,
        key: &BtrfsKey,
        node: &mut [u8],
    ) -> Result<Option<BtrfsKey>, BtrfsError> {
        let leaf = self.seek(device, root, key, node)?;
        if let Some(found) = at_or_before(&leaf, key) {
            return Ok(Some(found));
        }
        // Nothing in this leaf is `<= key`. The answer, if any, is the last
        // item of the previous leaf, which is the leaf holding the key just
        // below this subtree's first one.
        let Some(before) = leaf.boundary.as_ref().and_then(predecessor) else {
            return Ok(None);
        };
        let previous = self.seek(device, root, &before, node)?;
        Ok(at_or_before(&previous, &before))
    }

    /// Read the node at `at` into `block` and check what its parent promised.
    fn read_node<D: Device>(
        &self,
        device: &mut D,
        at: TreeRoot,
        block: &mut [u8],
    ) -> Result<(), BtrfsError> {
        self.read_logical(device, at.bytenr, block)?;
        let header = NodeHeader::parse(block)?;
        if header.level != at.level
            || header.generation != at.generation
            || header.fsid != self.fsid
        {
            return Err(BtrfsError::BadTree { logical: at.bytenr });
        }
        Ok(())
    }

    /// Add every `CHUNK_ITEM` in the chunk tree to the map.
    ///
    /// The system chunk array only covers the chunks holding the chunk tree
    /// itself; data and metadata chunks are only described here.
    fn load_chunk_tree<D: Device>(
        &mut self,
        device: &mut D,
        node: &mut [u8],
    ) -> Result<(), BtrfsError> {
        let mut key = BtrfsKey::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, 0);
        loop {
            let leaf = self.seek(device, self.chunk_tree, &key, node)?;
            for item in leaf.items() {
                if item.key.item_type == CHUNK_ITEM_KEY {
                    self.chunks
                        .insert(item.key.offset, &ChunkItem::parse(item.data)?)?;
                }
            }
            match leaf.next {
                Some(next) => key = next,
                None => return Ok(()),
            }
        }
    }

    /// The `ROOT_ITEM` of tree `objectid` in the root tree.
    fn find_root<D: Device>(
        &self,
        device: &mut D,
        objectid: u64,
        node: &mut [u8],
    ) -> Result<RootItem, BtrfsError> {
        let key = BtrfsKey::new(objectid, ROOT_ITEM_KEY, 0);
        let found = self.walk(device, self.root_tree, key, node, |_, item| {
            if item.key.objectid != objectid || item.key.item_type != ROOT_ITEM_KEY {
                return Ok(ControlFlow::Break(None));
            }
            Ok(ControlFlow::Break(Some(RootItem::parse(item.data)?)))
        })?;
        found.flatten().ok_or(BtrfsError::MissingRoot(objectid))
    }
}

/// Refuse a volume this reader would read wrongly rather than not at all.
fn check_mountable(sb: &Superblock<'_>) -> Result<(), BtrfsError> {
    let unknown = sb.incompat_flags().unknown();
    if unknown != 0 {
        return Err(BtrfsError::UnsupportedFeature(unknown));
    }
    if sb.num_devices() != 1 {
        return Err(BtrfsError::MultipleDevices(sb.num_devices()));
    }
    // A log tree holds fsync'd changes not yet in the fs trees. Linux replays
    // it even for a read-only mount; reading without it would show files as
    // they were before the fsync that promised they were safe.
    if sb.log_root() != 0 {
        return Err(BtrfsError::UnreplayedLog);
    }
    Ok(())
}

/// The key of the last item of `leaf` that is `<= key`, if the leaf has one.
fn at_or_before(leaf: &Leaf<'_>, key: &BtrfsKey) -> Option<BtrfsKey> {
    let exact = leaf.node.key(leaf.slot).filter(|found| found == key);
    exact.or_else(|| leaf.slot.checked_sub(1).and_then(|s| leaf.node.key(s)))
}

/// The greatest key strictly below `key`, or `None` for [`BtrfsKey::MIN`].
fn predecessor(key: &BtrfsKey) -> Option<BtrfsKey> {
    if let Some(offset) = key.offset.checked_sub(1) {
        return Some(BtrfsKey::new(key.objectid, key.item_type, offset));
    }
    if let Some(item_type) = key.item_type.checked_sub(1) {
        return Some(BtrfsKey::new(key.objectid, item_type, u64::MAX));
    }
    let objectid = key.objectid.checked_sub(1)?;
    Some(BtrfsKey::new(objectid, u8::MAX, u64::MAX))
}

#[cfg(test)]
pub(crate) mod tests;
