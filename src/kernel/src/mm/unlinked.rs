//! Page tables an unmap has unlinked from a live tree, kept from the frame
//! allocator until every processor that may have walked them has flushed
//! (finding F-36).
//!
//! # Why a table cannot go back when it empties
//!
//! A processor caches more than leaf translations. x86-64's paging-structure
//! caches and the Arm architectures' walk caches hold the *intermediate*
//! descriptors of a walk -- "the table for this 2 MiB lies at frame F" -- so
//! that the next miss nearby starts part-way down. Clearing the parent's
//! descriptor in memory does not reach those caches; only a TLB invalidation
//! of an address the table translated does. So between an unmap clearing the
//! descriptor and the shootdown returning, another processor running the
//! same address space may still walk *through the table it cached*. If the
//! table's frame had gone back to the allocator in that window and been handed
//! to somebody who wrote into it -- a user page of another thread, filled with
//! descriptors of its choosing -- that walk would read their bytes as
//! descriptors and translate to whatever physical memory they name. That is
//! isolation lost, not a crash.
//!
//! The kernel's own unmaps ([`super::unmap_kernel_all`]) always held their
//! tables until after the shootdown. The user half's did not: the callback of
//! `unmap_in` freed a table the moment it emptied, in phase one of the order
//! `user/space.rs` states, before the shootdown that phase three waits for.
//! Now a table an unmap empties is put on an [`UnlinkedTables`] list carried
//! by the shootdown's own [`crate::smp::TlbPages`], and
//! [`crate::smp::flush_tlb_pages`] gives the list back only once every
//! processor it reached has answered.
//!
//! # The list lives in the tables
//!
//! An unmap happens where running out of memory cannot be reported (finding
//! F-23), and a table list could need a slot for every table a large unmap
//! empties. So the list takes no memory: each unlinked table's first
//! descriptor holds the physical address of the next one. The table is
//! empty -- it is unlinked only because no descriptor in it is valid -- and
//! the value written is a frame address, page aligned, so its low two bits
//! are clear. Bit 0 clear is *not present* on x86-64 and in VT-d's second
//! level, and bits 1:0 clear is *invalid* in every Arm descriptor format,
//! long or short, stage 1 or stage 2. A stale walk that reaches the slot in
//! the window finds nothing, exactly as it would have before the slot was
//! written.
//!
//! # A list that is dropped
//!
//! Dropping a list that still holds tables means an unmap whose shootdown
//! never ran, or a unit that never said it had forgotten. Neither is known to
//! be safe to free after, so the tables are kept for good and counted in
//! [`TABLES_KEPT`], which the memory checks require to be zero.

use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_paging::{PhysAddr, PhysMem};

use super::{KernelPhysMem, Route, count, deallocate_frames};

/// Tables a dropped [`UnlinkedTables`] still held, and which were therefore
/// never given back. Zero on a correct kernel.
static TABLES_KEPT: AtomicU64 = AtomicU64::new(0);

/// How many tables were kept for good: see [`TABLES_KEPT`].
pub(crate) fn tables_kept() -> u64 {
    TABLES_KEPT.load(Ordering::Relaxed)
}

/// Where the tables a list gives back were counted: a user tree's or an
/// IOMMU domain's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// A user address space: counted as [`Route::UserTables`].
    User,
    /// An IOMMU domain: counted only as freed frames, as before.
    Device,
}

/// Page tables unlinked from a live tree and not yet given back.
///
/// See the module. Built empty, filled by the unmap, and emptied only by
/// [`UnlinkedTables::release`] after the flush that covers every address the
/// tables translated.
#[derive(Debug)]
pub(crate) struct UnlinkedTables {
    /// The first table's physical address; meaningless when `count` is zero.
    first: u64,
    /// How many tables are on the list.
    count: u64,
    /// Whose tables they are, for the route they are counted on.
    owner: Owner,
}

impl UnlinkedTables {
    /// An empty list of a user tree's tables.
    pub(crate) const fn new() -> UnlinkedTables {
        UnlinkedTables::of(Owner::User)
    }

    /// An empty list of `owner`'s tables.
    pub(crate) const fn of(owner: Owner) -> UnlinkedTables {
        UnlinkedTables {
            first: 0,
            count: 0,
            owner,
        }
    }

    /// How many tables it holds.
    pub(crate) const fn len(&self) -> u64 {
        self.count
    }

    /// Whether it holds none.
    pub(crate) const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Hold `table`, just unlinked, until [`UnlinkedTables::release`].
    ///
    /// Writes the link into the table's first descriptor, which is why the
    /// table must be one the unmap has just emptied: see the module.
    pub(crate) fn push(&mut self, table: PhysAddr) {
        let next = if self.count == 0 { 0 } else { self.first };
        KernelPhysMem.write(table, next);
        self.first = table.0;
        self.count += 1;
    }

    /// Take the most recently held table off, if there is one, putting its
    /// first descriptor back to the empty value a table is given back with.
    fn pop(&mut self) -> Option<PhysAddr> {
        if self.count == 0 {
            return None;
        }
        let table = PhysAddr(self.first);
        self.first = KernelPhysMem.read(table);
        KernelPhysMem.write(table, 0);
        self.count -= 1;
        Some(table)
    }

    /// Move every table `other` holds onto this list, for shootdowns merged
    /// into one: the merged flush reaches every address either covered.
    pub(crate) fn take_from(&mut self, other: &mut UnlinkedTables) {
        while let Some(table) = other.pop() {
            // NOALLOC: the list is linked through the tables themselves.
            self.push(table);
        }
    }

    /// Every table on the list, most recent first, without taking any off:
    /// for the check that they are still held.
    pub(crate) fn frames(&self) -> impl Iterator<Item = u64> + '_ {
        let mut at = self.first;
        (0..self.count).map(move |_| {
            let table = at;
            at = KernelPhysMem.read(PhysAddr(table));
            table / PAGE_SIZE
        })
    }

    /// Give every table back to the frame allocator.
    ///
    /// Only once no processor or unit can still walk them: after
    /// [`crate::smp::flush_tlb_pages`] for the addresses they translated
    /// returned, or after an IOMMU unit's invalidation completed. Called
    /// from anywhere else, this is the bug the module describes.
    pub(crate) fn release(&mut self) {
        while let Some(table) = self.pop() {
            if self.owner == Owner::User {
                count(Route::UserTables, 1);
            }
            deallocate_frames(table.0 / PAGE_SIZE, 0);
        }
    }
}

impl Drop for UnlinkedTables {
    /// Keep for good whatever was never released: see the module.
    fn drop(&mut self) {
        if self.count != 0 {
            let _ = TABLES_KEPT.fetch_add(self.count, Ordering::Relaxed);
        }
    }
}
