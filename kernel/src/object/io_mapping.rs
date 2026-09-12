//! I/O mappings: a device's registers, mappable into a driver's address
//! space, and nothing outside them.
//!
//! `docs/ARCHITECTURE.md` §7 gives a driver "an `IoMapping` for each BAR or
//! MMIO window, and nothing outside it". The "nothing outside" is enforced
//! before this module is reached: an [`IoMapping`] is built only from an
//! [`Aperture`], and only `crate::device` can make one, from what enumeration
//! found. This module's own rule is the one a page table forces on top.
//!
//! # Whole pages, or refused
//!
//! A mapping is made of pages, and an aperture need not be. QEMU's ARM
//! machines pack their virtio-mmio transports 0x200 bytes apart, several to a
//! page, so rounding an aperture out to its page would hand a driver its
//! neighbours' registers as well as its own — exactly what the aperture type
//! exists to prevent. So an aperture that is not a whole number of pages is
//! refused rather than rounded. Sub-page apertures need trapping each access,
//! which is a later story; drivers on those machines use virtio-pci, whose
//! BARs are whole pages.

use alloc::sync::Arc;

use ferrix_vma::VmaFlags;

use crate::device::Aperture;
use crate::user::space::{AddressSpace, SpaceError};

/// Why an I/O mapping could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IoMappingError {
    /// The aperture does not start on a page boundary or is not a whole
    /// number of pages, so no page mapping covers it and nothing else.
    NotWholePages,
}

/// A device aperture a driver may map.
#[derive(Debug)]
pub(crate) struct IoMapping {
    /// What it covers, exactly.
    aperture: Aperture,
}

impl IoMapping {
    /// An I/O mapping of `aperture`.
    ///
    /// # Errors
    ///
    /// [`IoMappingError::NotWholePages`].
    pub(crate) fn new(aperture: Aperture) -> Result<Arc<IoMapping>, IoMappingError> {
        if !aperture.whole_pages() {
            return Err(IoMappingError::NotWholePages);
        }
        Ok(Arc::new(IoMapping { aperture }))
    }

    /// Map it into `space`, at `at` or wherever it fits, readable and
    /// writable. Returns where.
    ///
    /// # Errors
    ///
    /// Whatever [`AddressSpace::map_device`] refuses.
    pub(crate) fn map_into(
        &self,
        space: &AddressSpace,
        at: Option<u64>,
    ) -> Result<u64, SpaceError> {
        space.map_device(
            at,
            self.aperture.len(),
            self.aperture.phys(),
            VmaFlags::READ_WRITE,
        )
    }
}
