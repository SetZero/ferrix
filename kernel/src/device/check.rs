//! Stage 10's device node checks, run by [`super::publish`] on every boot
//! before the nodes are published: each node hands out exactly the
//! apertures and vectors it has and nothing past them, no two nodes share
//! memory or an interrupt, and the first MSI-X table's vector behaves as one.
//!
//! Moved here from `device.rs`, the bodies unchanged, so that each can name
//! the requirement it verifies (`docs/sysml/20-device-requirements.sysml`):
//! check code in a product file can carry no tag.

use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use ferrix_pci::header::{COMMAND, COMMAND_BUS_MASTER, COMMAND_MEMORY_SPACE};

use super::{
    DeviceNode, FIRST_SHARED_INTERRUPT, Failure, LEGACY_CONFIG_BYTES, Location, Report, Reserved,
    Trigger,
};
use crate::mmio::Mmio;
use crate::{irq, vmap};

/// Mint one PCI node's first MSI-X vector and require it to behave as one:
/// the same vector when asked again, no handler already on its number, an
/// entry that reads back unmasked and masked as told, and nothing minted past
/// the table.
///
/// Runs on a published node, because minting needs the node's place in
/// [`devices`], and on one only, because the vector is spent for good. The
/// entry is left masked; a driver that asks for entry 0 gets this vector.
///
/// Verifies: L.device.6, L.device.7
pub(super) fn check_msix(node: &DeviceNode, report: &mut Report) -> Result<(), Failure> {
    let fail = |what| Failure {
        location: node.location(),
        what,
    };
    let Some(table) = &node.msix else {
        return Ok(());
    };
    if node.vector(node.vector_count()).is_some() {
        return Err(fail("a vector past the MSI-X table was minted"));
    }
    report.refusals += 1;
    let Some(first) = node.vector(0) else {
        // No vector to give, as on a machine without an MSI frame: nothing
        // to check, and no rule broken.
        return Ok(());
    };
    report.msix_minted += 1;
    if node.vector(0) != Some(first) {
        return Err(fail("an MSI-X entry was minted twice as different vectors"));
    }
    if irq::is_registered(first.number()) {
        return Err(fail(
            "an MSI-X vector was minted on a number with a handler",
        ));
    }
    if table.is_masked(0) != Some(true) {
        return Err(fail("a minted MSI-X entry was not masked"));
    }
    let unmasked = first.set_masked(false).map(|()| table.is_masked(0));
    let masked = first.set_masked(true).map(|()| table.is_masked(0));
    if unmasked != Ok(Some(false)) || masked != Ok(Some(true)) {
        return Err(fail("a minted MSI-X entry did not mask as told"));
    }
    Ok(())
}

/// Require no two apertures anywhere to overlap, no vector to be held twice,
/// and every device tree vector to be a shared peripheral interrupt.
///
/// A second pass over what `publish` built rather than a restatement of how it
/// built it: this sorts every aperture of every node and compares neighbours.
///
/// Verifies: L.device.5
pub(super) fn check_exclusive(nodes: &[DeviceNode]) -> Result<(), Failure> {
    let mut all: Vec<(u64, u64, Location)> = nodes
        .iter()
        .flat_map(|node| {
            node.apertures
                .iter()
                .map(move |aperture| (aperture.phys, aperture.end(), node.location))
        })
        // FATAL-ALLOC: boot only: stage 10 builds the device registry once, before any program runs.
        .collect();
    all.sort_unstable_by_key(|&(start, ..)| start);
    for pair in all.windows(2) {
        if let [(_, end, _), (start, _, location)] = pair
            && start < end
        {
            return Err(Failure {
                location: *location,
                what: "two apertures overlap",
            });
        }
    }

    let mut numbers = BTreeSet::new();
    for node in nodes {
        for vector in &node.vectors {
            if matches!(node.location, Location::VirtioMmio(_) | Location::Tree(_))
                && vector.number < FIRST_SHARED_INTERRUPT
            {
                return Err(Failure {
                    location: node.location,
                    what: "a vector is not a shared peripheral interrupt",
                });
            }
            // FATAL-ALLOC: boot only: stage 10 builds the device registry once, before any program runs.
            if !numbers.insert(vector.number) {
                return Err(Failure {
                    location: node.location,
                    what: "two nodes hold one vector",
                });
            }
        }
    }
    Ok(())
}

/// Require one node's tokens to follow the rule.
///
/// Verifies: L.device.1, L.device.2, L.device.3, L.device.4
pub(super) fn check_node(
    node: &DeviceNode,
    reserved: &Reserved,
    report: &mut Report,
) -> Result<(), Failure> {
    let fail = |what| Failure {
        location: node.location(),
        what,
    };
    report.withheld += node.withheld;
    report.vectors_withheld += node.withheld_vectors;
    if node.undecoded {
        report.undecoded += 1;
    }

    for &whole in node.apertures() {
        report.apertures += 1;
        if !whole.whole_pages() {
            report.partial_pages += 1;
        }
        if reserved.covers(whole) {
            return Err(fail("an aperture overlaps memory the kernel uses"));
        }
        if node.aperture(whole.phys(), whole.len()) != Some(whole) {
            return Err(fail("an aperture did not authorise itself"));
        }
        let last = whole.end() - 1;
        if node.aperture(last, 1).is_none() {
            return Err(fail("an aperture's last byte was refused"));
        }
        // Past the end: one byte inside and one outside, which must be refused
        // even when another aperture starts at the next byte — a mapping lies
        // inside one aperture or it is not a mapping of this device.
        let refused = [
            node.aperture(last, 2),
            node.aperture(whole.phys(), 0),
            whole
                .phys()
                .checked_sub(1)
                .and_then(|below| node.aperture(below, 2)),
        ];
        if refused.iter().any(Option::is_some) {
            return Err(fail("an aperture was granted past its edge"));
        }
        report.refusals += refused.len();
    }
    if node.aperture(u64::MAX, 2).is_some() {
        return Err(fail("an aperture wrapped the address space"));
    }
    report.refusals += 1;

    // A PCI node's vectors are minted on demand and only once it is
    // published, so asking here would neither mint nor check anything;
    // `check_msix` does that after publishing.
    if node.msix.is_none() {
        for (index, &vector) in node.vectors.iter().enumerate() {
            report.vectors += 1;
            if vector.trigger() == Some(Trigger::Edge) {
                report.edge += 1;
            }
            if node.vector(index) != Some(vector) {
                return Err(fail("a vector was not handed out as recorded"));
            }
        }
        if node.vector(node.vector_count()).is_some() {
            return Err(fail("a vector past the end was handed out"));
        }
        report.refusals += 1;
    }
    for &(start, len) in &node.interrupt_tables {
        report.msix_withheld += 1;
        // The whole range, its first byte and its last: a driver that could
        // reach any of them could rewrite which interrupt the device raises.
        let refused = [
            node.aperture(start, len),
            node.aperture(start, 1),
            node.aperture(start + (len - 1), 1),
        ];
        if refused.iter().any(Option::is_some) {
            return Err(fail("an MSI-X table or pending-bit page was granted"));
        }
        report.refusals += refused.len();
    }
    Ok(())
}

/// A PCI function's bus mastering is what `enable_dma` and `disable_dma`
/// say, read back from its command register rather than taken from the
/// node's own flag: set after the one, with memory decoding on; clear after
/// the other, with memory decoding still on. So a quiesce's `disable_dma`
/// does stop the device reaching memory. The register and the flag are left
/// as they were found, whatever firmware set.
///
/// On the first published function with a configuration space; a machine
/// with none has nothing to switch.
///
/// Verifies: L.device.9
pub(super) fn check_dma_switch(nodes: &[Arc<DeviceNode>]) -> Result<(), Failure> {
    let Some((node, config_phys)) = nodes.iter().find_map(|node| {
        let phys = node
            .pci
            .as_ref()
            .and_then(|function| function.config_phys)?;
        Some((node, phys))
    }) else {
        return Ok(());
    };
    let fail = |what| Failure {
        location: node.location(),
        what,
    };
    let unmappable = fail("a function's configuration space could not be mapped to read it");
    let config = vmap::map_device(config_phys, LEGACY_CONFIG_BYTES).map_err(|_| unmappable)?;
    let registers = Mmio::at(config);
    let at = u64::from(COMMAND);
    let found = registers.read16(at);
    let was_on = node.dma_on.load(Ordering::Acquire);

    let switched = (|| {
        node.enable_dma()
            .map_err(|_| fail("a function's DMA could not be turned on"))?;
        let on = registers.read16(at);
        if on & COMMAND_BUS_MASTER == 0 || on & COMMAND_MEMORY_SPACE == 0 {
            return Err(fail(
                "DMA turned on left the function's bus mastering or decoding off",
            ));
        }
        node.disable_dma()
            .map_err(|_| fail("a function's DMA could not be turned off"))?;
        let off = registers.read16(at);
        if off & COMMAND_BUS_MASTER != 0 {
            return Err(fail("DMA turned off left the function's bus mastering on"));
        }
        if off & COMMAND_MEMORY_SPACE == 0 {
            return Err(fail(
                "turning a function's DMA off turned its memory decoding off",
            ));
        }
        Ok(())
    })();

    registers.write16(at, found);
    node.dma_on.store(was_on, Ordering::Release);
    let _ = vmap::unmap_device(config);
    switched
}
