//! One memory type per page of device memory, across every user mapping.
//!
//! A device page reaches ring 3 three ways: `io_mapping_map` maps an
//! aperture as device memory (uncacheable), `io_mapping_map_combining` maps a
//! prefetchable one write-combining, and `mmap` of a render node's blob maps
//! a window of the device's memory cacheable when the device says it may be
//! cached (`syscall::memory::map_window`). Two of those over one page at once
//! are an alias of two memory types. The SDM (Vol. 3A §11.12.4) forbids a
//! write-combining page aliasing a cacheable one, because write-combining
//! stores need not snoop the caches, and the consequence can be a machine
//! check that is not confined to the device; on Arm, Normal non-cacheable
//! beside Normal write-back is a mismatched-attribute alias that loses
//! coherence. Uncacheable beside write-combining is not forbidden, but it
//! is not needed either, and one rule is easier to hold than two.
//!
//! So every user mapping of device memory holds a [`Hold`] on its physical
//! range for as long as any region of any address space maps it, and a
//! mapping whose type differs from a live hold's on any page of its range is
//! refused. The hold is kept beside the region's id, as a window's keeper
//! is, and dropped only after the unmap's shootdown has returned
//! (`AddressSpace::give_back`), so no processor can still hold a translation
//! of the old type when a mapping of the new one is allowed.
//!
//! The kernel's own mappings are not here: its direct map covers RAM runs
//! only, so it holds no alias of a device page, and the framebuffer it maps
//! itself is not an aperture a driver is given.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::fallible;
use crate::sync::SpinLock;

/// How a processor maps a page of device memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MemoryType {
    /// Device memory, uncacheable: an aperture of registers, or a window the
    /// device did not say may be cached.
    Device,
    /// Write-combining: a prefetchable aperture a driver asked to combine.
    Combining,
    /// Ordinary cacheable memory: a window the device said may be cached.
    Cached,
}

impl MemoryType {
    /// The type a device region with these attributes is mapped with: what
    /// `AddressSpace::fault_device` puts in the page table.
    pub(crate) const fn of(cached: bool, combining: bool) -> MemoryType {
        if cached {
            MemoryType::Cached
        } else if combining {
            MemoryType::Combining
        } else {
            MemoryType::Device
        }
    }
}

/// Why a hold was not given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Refused {
    /// A page of the range is mapped with another type now.
    OtherType,
    /// There was no memory to record it.
    NoMemory,
}

/// One live hold: `start..end` mapped as `kind`.
#[derive(Clone, Copy, Debug)]
struct Claim {
    serial: u64,
    start: u64,
    end: u64,
    kind: MemoryType,
}

/// Every live hold. A leaf lock: taken under an address space's lock by a
/// mapping, and on its own when a hold drops; nothing is taken inside it.
/// Holds no more entries than there are live device mappings.
static CLAIMS: SpinLock<Vec<Claim>> = SpinLock::new(Vec::new());

/// The next hold's serial: what its drop finds its entry by.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// A mapping's claim on the memory type of its physical range: given back
/// when the last region holding it goes.
#[derive(Debug)]
pub(crate) struct Hold {
    serial: u64,
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut claims = CLAIMS.lock();
        if let Some(at) = claims.iter().position(|claim| claim.serial == self.serial) {
            let _ = claims.swap_remove(at);
        }
    }
}

/// Hold `physical..physical + len` as `kind`, unless a page of it is held as
/// another type.
///
/// The caller has checked the range is whole pages and does not wrap.
///
/// # Errors
///
/// [`Refused::OtherType`] when a live hold of another type overlaps the
/// range, and [`Refused::NoMemory`].
pub(crate) fn hold(physical: u64, len: u64, kind: MemoryType) -> Result<Arc<Hold>, Refused> {
    let end = physical.checked_add(len).ok_or(Refused::OtherType)?;
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    // Made before the entry: dropping it on any refusal below removes
    // nothing, and once the entry is in, its drop is what removes it.
    let held = fallible::try_arc(Hold { serial }).map_err(|_| Refused::NoMemory)?;
    let mut claims = CLAIMS.lock();
    let other = claims
        .iter()
        .any(|claim| claim.kind != kind && claim.start < end && physical < claim.end);
    let recorded = if other {
        Err(Refused::OtherType)
    } else {
        fallible::try_push(
            &mut claims,
            Claim {
                serial,
                start: physical,
                end,
                kind,
            },
        )
        .map_err(|_| Refused::NoMemory)
    };
    // Unlocked before `held` can drop: its drop takes this lock.
    drop(claims);
    recorded.map(|()| held)
}
