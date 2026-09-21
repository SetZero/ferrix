//! Tree nodes as the writer holds them: parsed, edited as lists, written whole.
//!
//! On disk a leaf is a descriptor array growing forward and payloads packed
//! backward from the end (see `ferrix_btrfs::tree`). Editing that in place
//! means shifting both regions and rewriting offsets on every insert, which is
//! exactly where an off-by-one produces a leaf that still checksums but whose
//! items overlap. So a node being changed is held here as what it means — a
//! sorted list of items, or of child pointers — and turned back into bytes in
//! one place, [`TreeNode::write`], which lays every payload out from scratch.
//!
//! A node may hold more than fits while an edit is in progress; the tree code
//! splits it with [`TreeNode::split_points`] before anything is written.

use alloc::vec::Vec;
use core::ops::Range;

use ferrix_btrfs::crc32c;
use ferrix_btrfs::tree::{BtrfsKey, HEADER_SIZE, ITEM_SIZE, KEY_PTR_SIZE, KeyPtr, Node};

use crate::bytes::{put, put_key, put_u8, put_u32, put_u64};
use crate::{Error, Result, Unsupported};

/// Header flag: the node has been written to disk at least once.
pub const HEADER_FLAG_WRITTEN: u64 = 1 << 0;
/// Header flag: the node belongs to a relocation tree.
pub const HEADER_FLAG_RELOC: u64 = 1 << 1;
/// The back-reference revision every node written since 2.6.31 carries, in
/// the top byte of the header flags: back-references name the owning tree.
pub const MIXED_BACKREF_REV: u64 = 1;
/// Where the back-reference revision sits in the header flags.
const BACKREF_REV_SHIFT: u32 = 56;

/// One leaf item: its key and its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafItem {
    /// The key the leaf is sorted by.
    pub key: BtrfsKey,
    /// The payload, whatever the key's type says it is.
    pub data: Vec<u8>,
}

/// What a node holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// A leaf's items, in key order.
    Leaf(Vec<LeafItem>),
    /// An internal node's child pointers, in key order.
    Internal(Vec<KeyPtr>),
}

/// A node being read or changed by a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    /// The logical address the node lives at, and records in its header.
    pub bytenr: u64,
    /// The transaction that wrote it.
    pub generation: u64,
    /// The tree it belongs to.
    pub owner: u64,
    /// Height above the leaves.
    pub level: u8,
    /// Items or pointers.
    pub body: Body,
}

impl TreeNode {
    /// A leaf with nothing in it: how an empty tree's root looks.
    #[must_use]
    pub const fn empty_leaf(bytenr: u64, generation: u64, owner: u64) -> Self {
        TreeNode {
            bytenr,
            generation,
            owner,
            level: 0,
            body: Body::Leaf(Vec::new()),
        }
    }

    /// Copy a node `ferrix-btrfs` has parsed and checked into the editable
    /// form.
    ///
    /// A node carrying the relocation flag is refused: it belongs to a
    /// balance in progress, whose trees share blocks with the ones it moves.
    pub fn from_node(node: &Node<'_>) -> Result<Self> {
        let header = node.header();
        if header.flags & HEADER_FLAG_RELOC != 0 {
            return Err(Error::Unsupported(Unsupported::SharedBlock));
        }
        let body = if node.is_leaf() {
            Body::Leaf(
                node.items()
                    .map(|item| LeafItem {
                        key: item.key,
                        data: item.data.to_vec(),
                    })
                    .collect(),
            )
        } else {
            Body::Internal(node.key_ptrs().collect())
        };
        Ok(TreeNode {
            bytenr: header.bytenr,
            generation: header.generation,
            owner: header.owner,
            level: header.level,
            body,
        })
    }

    /// How many items or pointers the node holds.
    #[must_use]
    pub fn nritems(&self) -> usize {
        match &self.body {
            Body::Leaf(items) => items.len(),
            Body::Internal(ptrs) => ptrs.len(),
        }
    }

    /// Whether the node holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nritems() == 0
    }

    /// The smallest key in the node, which is what its parent's pointer to it
    /// must say.
    #[must_use]
    pub fn first_key(&self) -> Option<BtrfsKey> {
        match &self.body {
            Body::Leaf(items) => items.first().map(|item| item.key),
            Body::Internal(ptrs) => ptrs.first().map(|ptr| ptr.key),
        }
    }

    /// The key at `slot`.
    #[must_use]
    pub fn key(&self, slot: usize) -> Option<BtrfsKey> {
        match &self.body {
            Body::Leaf(items) => items.get(slot).map(|item| item.key),
            Body::Internal(ptrs) => ptrs.get(slot).map(|ptr| ptr.key),
        }
    }

    /// Bytes of the node body in use: a descriptor and a payload per leaf
    /// item, a key pointer per child.
    #[must_use]
    pub fn used(&self) -> usize {
        match &self.body {
            Body::Leaf(items) => items
                .iter()
                .map(|item| ITEM_SIZE.saturating_add(item.data.len()))
                .fold(0usize, usize::saturating_add),
            Body::Internal(ptrs) => ptrs.len().saturating_mul(KEY_PTR_SIZE),
        }
    }

    /// Bytes of body a node of this kind can hold on a volume with `nodesize`
    /// nodes. An internal node holds whole key pointers only.
    #[must_use]
    pub fn capacity(&self, nodesize: u32) -> usize {
        let body = (nodesize as usize).saturating_sub(HEADER_SIZE);
        match self.body {
            Body::Leaf(_) => body,
            Body::Internal(_) => body - body % KEY_PTR_SIZE,
        }
    }

    /// Whether the node holds more than one node can.
    #[must_use]
    pub fn overflows(&self, nodesize: u32) -> bool {
        self.used() > self.capacity(nodesize)
    }

    /// Where to cut an overfull node so that every piece fits.
    ///
    /// The pieces are of about equal size rather than filled in turn, so that
    /// a leaf filled by appending does not split into a full leaf and a
    /// nearly empty one, only to split again on the next insert. `None` if a
    /// single item is larger than a whole node.
    #[must_use]
    pub fn split_points(&self, nodesize: u32) -> Option<Vec<Range<usize>>> {
        let capacity = self.capacity(nodesize);
        let sizes: Vec<usize> = match &self.body {
            Body::Leaf(items) => items
                .iter()
                .map(|item| ITEM_SIZE.saturating_add(item.data.len()))
                .collect(),
            Body::Internal(ptrs) => ptrs.iter().map(|_| KEY_PTR_SIZE).collect(),
        };
        if capacity == 0 || sizes.iter().any(|&size| size > capacity) {
            return None;
        }
        let total = sizes
            .iter()
            .fold(0usize, |sum, &size| sum.saturating_add(size));
        let pieces = total.div_ceil(capacity).max(2);
        let target = total.div_ceil(pieces);
        let mut ranges = Vec::new();
        let (mut start, mut filled) = (0usize, 0usize);
        for (index, &size) in sizes.iter().enumerate() {
            let over = filled.saturating_add(size);
            if filled > 0 && (over > target || over > capacity) {
                ranges.push(start..index);
                start = index;
                filled = 0;
            }
            filled = filled.saturating_add(size);
        }
        ranges.push(start..sizes.len());
        Some(ranges)
    }

    /// Cut the node at `points`, keeping the first piece and returning the
    /// bodies of the rest in order.
    pub fn split_off(&mut self, points: &[Range<usize>]) -> Vec<Body> {
        let mut rest = Vec::new();
        for range in points.iter().skip(1).rev() {
            let body = match &mut self.body {
                Body::Leaf(items) => Body::Leaf(items.split_off(range.start)),
                Body::Internal(ptrs) => Body::Internal(ptrs.split_off(range.start)),
            };
            rest.push(body);
        }
        rest.reverse();
        rest
    }

    /// Serialise the node into `out`, which must be exactly one node long.
    ///
    /// Everything outside the header, descriptors and payloads is zero, so the
    /// same node always produces the same bytes. The checksum is computed
    /// last, over everything after its own field. `None` if the node does not
    /// fit, which the tree code never lets happen.
    pub fn write(&self, out: &mut [u8], fsid: &[u8; 16], chunk_tree_uuid: &[u8; 16]) -> Option<()> {
        out.fill(0);
        let nritems = u32::try_from(self.nritems()).ok()?;
        let flags = HEADER_FLAG_WRITTEN | (MIXED_BACKREF_REV << BACKREF_REV_SHIFT);
        put(out, 32, fsid)?;
        put_u64(out, 48, self.bytenr)?;
        put_u64(out, 56, flags)?;
        put(out, 64, chunk_tree_uuid)?;
        put_u64(out, 80, self.generation)?;
        put_u64(out, 88, self.owner)?;
        put_u32(out, 96, nritems)?;
        put_u8(out, 100, self.level)?;
        match &self.body {
            Body::Leaf(items) => write_leaf(out, items)?,
            Body::Internal(ptrs) => {
                for (slot, ptr) in ptrs.iter().enumerate() {
                    let at = HEADER_SIZE.checked_add(slot.checked_mul(KEY_PTR_SIZE)?)?;
                    put_key(out, at, &ptr.key)?;
                    put_u64(out, at.checked_add(17)?, ptr.blockptr)?;
                    put_u64(out, at.checked_add(25)?, ptr.generation)?;
                }
            }
        }
        let sum = crc32c(out.get(ferrix_btrfs::CSUM_SIZE..)?);
        put_u32(out, 0, sum)
    }
}

/// Lay out a leaf's descriptors forward from the header and its payloads
/// backward from the end, item 0's payload last in the node.
fn write_leaf(out: &mut [u8], items: &[LeafItem]) -> Option<()> {
    let mut end = out.len().checked_sub(HEADER_SIZE)?;
    let descriptors = HEADER_SIZE.checked_add(items.len().checked_mul(ITEM_SIZE)?)?;
    for (slot, item) in items.iter().enumerate() {
        let size = item.data.len();
        end = end.checked_sub(size)?;
        if HEADER_SIZE.checked_add(end)? < descriptors {
            return None;
        }
        let at = HEADER_SIZE.checked_add(slot.checked_mul(ITEM_SIZE)?)?;
        put_key(out, at, &item.key)?;
        put_u32(out, at.checked_add(17)?, u32::try_from(end).ok()?)?;
        put_u32(out, at.checked_add(21)?, u32::try_from(size).ok()?)?;
        put(out, HEADER_SIZE.checked_add(end)?, &item.data)?;
    }
    Some(())
}
