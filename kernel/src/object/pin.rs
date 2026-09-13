//! Pins: pages of a VMO a device may reach, held until the handle is closed.
//!
//! `docs/ARCHITECTURE.md` §7 gives a driver "a `Vmo` for DMA, whose device
//! addresses come from an IOMMU domain scoped to that device". A pin is that
//! grant. `Vmo::hold` keeps the pages where they are, and the device's domain
//! maps them and says at which addresses the device reaches them.
//!
//! # The order a pin is given back in
//!
//! Out of the domain, and forgotten by the unit, first; only then are the
//! holds released and the frames free to go. On a translated domain that is
//! the whole story. On an untranslated one the device can still reach the
//! frames once they are unpinned, since nothing stands between it and memory,
//! so the frames stay held until the device is reset. Nothing resets a device
//! yet, so today they stay held for good, and the console says so the first
//! time. A pin its domain refuses to give back keeps its frames the same way.

use alloc::sync::Arc;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use ferrix_paging::MapFlags;

use crate::iommu::{Domain, DomainError, Pinned};
use crate::println;
use crate::user::vmo::Held;

/// Whether a kept pin has been announced.
static KEPT: AtomicBool = AtomicBool::new(false);

/// Pages of a VMO pinned into a device's domain.
pub(crate) struct Pin {
    /// The domain they are pinned into.
    domain: Arc<Domain>,
    /// The domain's record of them. Taken on drop.
    pinned: Option<Pinned>,
    /// The VMO's hold on them. Taken on drop.
    held: Option<Held>,
}

impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pin")
            .field("pages", &self.addresses().len())
            .field("translated", &self.domain.translated())
            .finish_non_exhaustive()
    }
}

impl Pin {
    /// Pin `held`'s pages into `domain`, writable when `flags` says so.
    ///
    /// # Errors
    ///
    /// What the domain refused. The hold is then released: nothing was mapped.
    pub(crate) fn new(
        domain: Arc<Domain>,
        held: Held,
        flags: MapFlags,
    ) -> Result<Pin, DomainError> {
        let pinned = domain.pin(held.frames(), flags)?;
        Ok(Pin {
            domain,
            pinned: Some(pinned),
            held: Some(held),
        })
    }

    /// Each page's device address, in page order.
    pub(crate) fn addresses(&self) -> &[u64] {
        self.pinned.as_ref().map_or(&[], Pinned::addresses)
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        let (Some(pinned), Some(held)) = (self.pinned.take(), self.held.take()) else {
            return;
        };
        let freeable = match self.domain.unpin(pinned) {
            Ok(()) => self.domain.translated(),
            Err((_, back)) => {
                back.leak();
                false
            }
        };
        if freeable {
            drop(held);
            return;
        }
        let _ = core::mem::ManuallyDrop::new(held);
        if !KEPT.swap(true, Ordering::Relaxed) {
            println!(
                "  iommu    a pin was closed while its device may still reach its pages: \
                 the frames are kept until the device is reset"
            );
        }
    }
}
