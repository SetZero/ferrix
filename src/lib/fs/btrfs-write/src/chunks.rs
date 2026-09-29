//! The chunk layout, with every stripe: where each copy of a logical range is.
//!
//! `ferrix_btrfs::chunk::ChunkMap` keeps one stripe per chunk, which is all a
//! reader needs: every stripe of a mirrored profile is a whole copy. A writer
//! needs all of them, because a DUP chunk written through its first stripe
//! alone leaves a second copy that disagrees, and a later read that falls back
//! to it — or a scrub — finds a node from an older transaction.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use ferrix_btrfs::BtrfsError;
use ferrix_btrfs::chunk::{
    BLOCK_GROUP_DUP, BLOCK_GROUP_PROFILE_MASK, BLOCK_GROUP_TYPE_MASK, ChunkItem, Stripe,
};

use crate::bytes::{put, put_u16, put_u32, put_u64};
use crate::{Error, Result, Unsupported};

/// One chunk: a logical range and the physical places it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chunk {
    /// First logical address.
    pub(crate) logical: u64,
    /// Length of the logical range, and of each stripe.
    pub(crate) length: u64,
    /// Type and profile bits.
    pub(crate) type_bits: u64,
    /// Every copy.
    pub(crate) stripes: Vec<Stripe>,
}

impl Chunk {
    /// A chunk from its item, filed at logical address `logical`.
    ///
    /// Only `SINGLE` (one stripe) and `DUP` (two, on the one device) are
    /// writable: every other profile either stripes or spans devices.
    pub(crate) fn from_item(logical: u64, item: &ChunkItem<'_>) -> Result<Chunk> {
        let profile = item.type_bits() & BLOCK_GROUP_PROFILE_MASK;
        let stripes: Vec<Stripe> = item.stripes().collect();
        let copies = if profile == BLOCK_GROUP_DUP { 2 } else { 1 };
        if (profile != 0 && profile != BLOCK_GROUP_DUP) || stripes.len() != copies {
            return Err(Error::Unsupported(Unsupported::Profile));
        }
        Ok(Chunk {
            logical,
            length: item.length(),
            type_bits: item.type_bits(),
            stripes,
        })
    }

    /// Logical end, exclusive.
    pub(crate) fn end(&self) -> u64 {
        self.logical.saturating_add(self.length)
    }

    /// The type bits alone: data, metadata or system.
    pub(crate) fn kind(&self) -> u64 {
        self.type_bits & BLOCK_GROUP_TYPE_MASK
    }

    /// The `CHUNK_ITEM` payload for this chunk, as the kernel writes one for
    /// a single-device volume.
    pub(crate) fn item_bytes(&self, sectorsize: u32) -> Option<Vec<u8>> {
        let num = u16::try_from(self.stripes.len()).ok()?;
        let mut out =
            alloc::vec![0u8; 48usize.checked_add(32usize.checked_mul(self.stripes.len())?)?];
        put_u64(&mut out, 0, self.length)?;
        // The owner is always the extent tree's id, for historical reasons.
        put_u64(&mut out, 8, ferrix_btrfs::items::EXTENT_TREE_OBJECTID)?;
        put_u64(&mut out, 16, STRIPE_LEN)?;
        put_u64(&mut out, 24, self.type_bits)?;
        put_u32(&mut out, 32, STRIPE_LEN as u32)?;
        put_u32(&mut out, 36, STRIPE_LEN as u32)?;
        put_u32(&mut out, 40, sectorsize)?;
        put_u16(&mut out, 44, num)?;
        put_u16(&mut out, 46, 1)?;
        for (index, stripe) in self.stripes.iter().enumerate() {
            let at = 48usize.checked_add(index.checked_mul(32)?)?;
            put_u64(&mut out, at, stripe.devid)?;
            put_u64(&mut out, at.checked_add(8)?, stripe.offset)?;
            put(&mut out, at.checked_add(16)?, &stripe.dev_uuid)?;
        }
        Some(out)
    }
}

/// The stripe length every chunk this crate makes records: 64 KiB, as Linux.
pub(crate) const STRIPE_LEN: u64 = 64 * 1024;

/// Every chunk of the volume, by logical address.
#[derive(Debug, Clone, Default)]
pub(crate) struct Chunks {
    by_logical: BTreeMap<u64, Chunk>,
}

impl Chunks {
    /// Add a chunk. The same chunk twice — the system chunk array repeats the
    /// chunk tree — is accepted once; a different chunk overlapping one
    /// already present is damage.
    pub(crate) fn insert(&mut self, chunk: Chunk) -> Result<()> {
        if let Some(existing) = self.by_logical.get(&chunk.logical) {
            return if *existing == chunk {
                Ok(())
            } else {
                Err(Error::Volume(BtrfsError::BadChunk))
            };
        }
        let before = self.by_logical.range(..chunk.logical).next_back();
        let after = self.by_logical.range(chunk.logical..).next();
        if before.is_some_and(|(_, prev)| prev.end() > chunk.logical)
            || after.is_some_and(|(&start, _)| start < chunk.end())
        {
            return Err(Error::Volume(BtrfsError::BadChunk));
        }
        let _ = self.by_logical.insert(chunk.logical, chunk);
        Ok(())
    }

    /// The chunk holding `logical`.
    pub(crate) fn lookup(&self, logical: u64) -> Option<&Chunk> {
        let (_, chunk) = self.by_logical.range(..=logical).next_back()?;
        (logical < chunk.end()).then_some(chunk)
    }

    /// Every physical offset holding `[logical, logical + len)`, first stripe
    /// first. The range must lie inside one chunk.
    pub(crate) fn copies(&self, logical: u64, len: u64) -> Result<Vec<u64>> {
        let chunk = self
            .lookup(logical)
            .ok_or(Error::Volume(BtrfsError::NotMapped(logical)))?;
        let within = logical - chunk.logical;
        if logical.checked_add(len).is_none_or(|end| end > chunk.end()) {
            return Err(Error::Volume(BtrfsError::NotMapped(chunk.end())));
        }
        chunk
            .stripes
            .iter()
            .map(|stripe| {
                stripe
                    .offset
                    .checked_add(within)
                    .ok_or(Error::Volume(BtrfsError::NotMapped(logical)))
            })
            .collect()
    }

    /// Every chunk, in logical order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Chunk> {
        self.by_logical.values()
    }

    /// Where the next chunk's logical range starts: after the last one, as
    /// Linux's `find_next_chunk`.
    pub(crate) fn next_logical(&self) -> u64 {
        self.by_logical.values().next_back().map_or(0, Chunk::end)
    }
}
