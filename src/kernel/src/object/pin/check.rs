//! The quarantine's boot check, beside the pins it drives: it reads what
//! `pin.rs` keeps to itself -- a domain's quarantined pages, the release, a
//! pin under a cap of its own -- as a child module may.

use alloc::sync::Arc;

use ferrix_paging::MapFlags;

use super::{Pin, PinError, quarantined_pages, release};
use crate::device::DeviceNode;
use crate::iommu::Domain;
use crate::object::process::Exit;

/// Drive the first translated PCI domain's quarantine to a cap of one page,
/// and require the next pin to be refused, and one to be taken again once the
/// quarantine is released: the cap's rule, at a size a boot can afford. With
/// no translated domain there is no quarantine, and nothing is checked.
///
/// # Errors
///
/// The first thing that is not so.
///
/// Verifies: L.object.47, L.object.48, L.object.49, H.DMA.4
pub(crate) fn check_quarantine(nodes: &[Arc<DeviceNode>]) -> Result<bool, &'static str> {
    let Some(domain) = translated_pci_domain(nodes)? else {
        return Ok(false);
    };
    let dead = Exit::for_check(true).map_err(|_| "no memory for a check's end")?;
    let live = Exit::for_check(false).map_err(|_| "no memory for a check's end")?;
    let before = quarantined_pages(&domain);
    drop(check_pin(&domain, &dead).map_err(|_| "a pin under the cap was refused")?);
    if quarantined_pages(&domain) != before + 1 {
        return Err("a pin closed by a dead process was not quarantined");
    }
    if !matches!(check_pin(&domain, &live), Err(PinError::QuarantineFull)) {
        return Err("a pin was taken on a domain whose quarantine was at its cap");
    }
    if release(&domain).pages != before + 1 || quarantined_pages(&domain) != 0 {
        return Err("a release left pages in the quarantine");
    }
    // Closed by a live process: given back at once, not quarantined.
    drop(check_pin(&domain, &live).map_err(|_| "a pin after the release was refused")?);
    if quarantined_pages(&domain) != 0 {
        return Err("a pin closed by a live process was quarantined");
    }
    Ok(true)
}

/// The first PCI node's domain, if it is translated.
fn translated_pci_domain(nodes: &[Arc<DeviceNode>]) -> Result<Option<Arc<Domain>>, &'static str> {
    let Some(node) = nodes
        .iter()
        .find(|node| matches!(node.location(), crate::device::Location::Pci(_)))
    else {
        return Ok(None);
    };
    let domain = node
        .domain()
        .map_err(|_| "no memory for a device's domain")?;
    Ok(domain.translated().then_some(domain))
}

/// A pin of one fresh page into `domain` for `owner`, under a cap of one
/// page, quietly: [`check_quarantine`]'s.
fn check_pin(domain: &Arc<Domain>, owner: &Arc<Exit>) -> Result<Pin, PinError> {
    let vmo = crate::user::vmo::Vmo::new_anonymous(1).map_err(|_| PinError::NoMemory)?;
    let held = vmo.hold(0, 1).map_err(|_| PinError::NoMemory)?;
    Pin::with_cap(
        Arc::clone(domain),
        held,
        MapFlags::DMA,
        Arc::clone(owner),
        1,
        true,
    )
}
