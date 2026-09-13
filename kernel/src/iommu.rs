//! Stage 10: where the IOMMUs are, and which one each PCI function's DMA
//! arrives at.
//!
//! Each firmware describes this its own way:
//!
//! * **The DMAR**, on x86-64: each VT-d unit's register block, and the
//!   endpoints behind it by bus, device and function. A device arrives at its
//!   unit as its requester ID.
//! * **The IORT**, on AArch64 under ACPI: a root complex's ID mappings,
//!   followed one hop to the `SMMUv3` that translates a requester ID, and the
//!   stream ID it arrives as.
//! * **The device tree**, on ARMv7-A: `arm,smmu-v3` nodes, and each ECAM host's
//!   `iommu-map` from requester IDs to a phandle and a stream ID.
//!
//! Nothing here programs a unit. What it finds is what a domain is built on;
//! until one is, the registers are only kept out of every driver's apertures,
//! and every function's DMA still reaches physical memory directly.
//!
//! # What cannot be followed is said, not guessed
//!
//! A DMAR scope that names a function through a bridge, a mapping that points
//! at a node or phandle that is not there, and a device tree host this cannot
//! tell apart from another are reported as unresolved rather than as behind
//! no IOMMU. The difference matters: a function counted as bypassing is one a
//! domain will never be built for.
//!
//! # Domains
//!
//! A [`Domain`] is the memory one device's DMA may reach, and the device
//! addresses it reaches it by: a driver pins pages into its device's domain
//! and gives the device the addresses the pin returns. Every domain is
//! untranslated for now — no unit is programmed, so a device address is the
//! physical address and a device can reach all of memory, which is the
//! degraded trusted mode `docs/ARCHITECTURE.md` §7 requires the kernel to
//! announce, and the first pin does. The VT-d and `SMMUv3` domains replace
//! what [`Domain::pin`] does behind the same signature.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferrix_acpi::dmar::{self, Structure};
use ferrix_acpi::iort;
use ferrix_bootinfo::{BootView, PAGE_SIZE};
use ferrix_fdt::{EcamHost, Fdt};
use ferrix_paging::MapFlags;
use ferrix_pci::Address;

use crate::device::{DeviceNode, Location};
use crate::{acpi, fdt, mm, println};

/// Bytes of a VT-d unit's registers kept from drivers: the first page, which
/// holds every register a legacy-mode driver uses. A DRHD gives no length.
const VTD_WINDOW: u64 = 0x1000;

/// Bytes of an `SMMUv3`'s registers: its two 64 KiB register pages. The IORT
/// gives no length.
const SMMU_V3_WINDOW: u64 = 0x2_0000;

/// A DMAR device scope for everything below a bridge.
const SCOPE_PCI_SUB_HIERARCHY: u8 = 0x02;

/// What kind of IOMMU a unit is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// An Intel VT-d remapping unit.
    VtD,
    /// An Arm `SMMUv3`.
    SmmuV3,
}

/// One IOMMU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Unit {
    /// What kind.
    pub(crate) kind: Kind,
    /// Physical address of its register block.
    pub(crate) phys: u64,
    /// Bytes of it no driver may be given.
    pub(crate) len: u64,
}

/// Where a function's DMA arrives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Placement {
    /// The function.
    pub(crate) function: Address,
    /// The unit, as an index into [`units`]' answer.
    pub(crate) unit: usize,
    /// What the unit sees it as: a VT-d source ID or an `SMMUv3` stream ID.
    pub(crate) stream: u32,
}

/// What firmware says about one function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Behind {
    /// A unit translates its DMA, seeing it as this stream.
    Unit {
        /// The unit's index.
        unit: usize,
        /// The stream or source ID.
        stream: u32,
    },
    /// No unit does.
    Nothing,
    /// Firmware says something this cannot follow.
    Unresolved,
}

/// Every IOMMU the machine describes, each once.
pub(crate) fn units(view: &BootView<'_>) -> Vec<Unit> {
    let mut units = Vec::new();
    if let Ok(firmware) = acpi::Firmware::open(view) {
        let tables = firmware.acpi();
        if let Ok(table) = tables.dmar() {
            for structure in table.structures() {
                if let Structure::Drhd(unit) = structure {
                    add(&mut units, Kind::VtD, unit.register_base, VTD_WINDOW);
                }
            }
        }
        if let Ok(table) = tables.iort() {
            for node in table.nodes() {
                if let Some(smmu) = node.smmu_v3() {
                    add(&mut units, Kind::SmmuV3, smmu.base_address, SMMU_V3_WINDOW);
                }
            }
        }
    } else if let Ok(tree) = fdt::open(view) {
        for smmu in tree.smmu_v3s() {
            add(
                &mut units,
                Kind::SmmuV3,
                smmu.region.address,
                smmu.region.size,
            );
        }
    }
    units
}

/// Record a unit, unless it is empty or already recorded.
fn add(units: &mut Vec<Unit>, kind: Kind, phys: u64, len: u64) {
    if phys != 0 && len != 0 && !units.iter().any(|unit| unit.phys == phys) {
        units.push(Unit { kind, phys, len });
    }
}

/// The index of the unit whose registers are at `phys`.
fn unit_at(units: &[Unit], phys: u64) -> Option<usize> {
    units.iter().position(|unit| unit.phys == phys)
}

/// A placement in `units`, or unresolved when the unit firmware named is not
/// among them.
fn behind(units: &[Unit], phys: u64, stream: u32) -> Behind {
    unit_at(units, phys).map_or(Behind::Unresolved, |unit| Behind::Unit { unit, stream })
}

/// Where the DMAR puts `function`.
///
/// Only single-hop endpoint scopes name a function this can match. A scope
/// through a bridge, or one covering everything below a bridge, on the
/// function's segment makes an unmatched function unresolved rather than
/// bypassing: it may be one of those.
fn place_dmar(table: &dmar::Dmar<'_>, units: &[Unit], function: Address) -> Behind {
    let endpoint = (function.bus(), function.device(), function.function());
    let mut unfollowed = false;
    for structure in table.structures() {
        let Structure::Drhd(unit) = structure else {
            continue;
        };
        if unit.segment != function.segment() {
            continue;
        }
        for scope in unit.device_scopes() {
            match scope.kind {
                dmar::SCOPE_PCI_ENDPOINT => match scope.endpoint() {
                    Some(found) if found == endpoint => {
                        return behind(
                            units,
                            unit.register_base,
                            u32::from(function.requester_id()),
                        );
                    }
                    Some(_) => {}
                    None => unfollowed = true,
                },
                SCOPE_PCI_SUB_HIERARCHY => unfollowed = true,
                _ => {}
            }
        }
    }
    if unfollowed {
        Behind::Unresolved
    } else {
        Behind::Nothing
    }
}

/// Where the IORT puts `function`: its segment's root complex, one mapping
/// on. A mapping to an ITS group is a function no `SMMUv3` translates.
fn place_iort(table: &iort::Iort<'_>, units: &[Unit], function: Address) -> Behind {
    let root = table.nodes().find(|node| {
        node.root_complex()
            .is_some_and(|complex| complex.segment == u32::from(function.segment()))
    });
    let Some(root) = root else {
        return Behind::Nothing;
    };
    let Some((stream, reference)) = root.translate(u32::from(function.requester_id())) else {
        return Behind::Nothing;
    };
    match table.node_at(reference) {
        None => Behind::Unresolved,
        Some(next) => match next.smmu_v3() {
            Some(smmu) => behind(units, smmu.base_address, stream),
            None if next.kind == iort::NODE_SMMU_V1_V2 => Behind::Unresolved,
            None => Behind::Nothing,
        },
    }
}

/// Where the device tree puts `function`: its host's `iommu-map`.
///
/// The host is the one naming the function's segment in `linux,pci-domain`,
/// or the only host when it names none; with several hosts and no domains the
/// kernel numbered the segments itself, and this does not guess which is
/// which.
fn place_tree(tree: &Fdt<'_>, units: &[Unit], function: Address) -> Behind {
    let hosts: Vec<EcamHost> = tree.ecam_hosts().collect();
    let named = hosts
        .iter()
        .find(|host| host.segment == Some(function.segment()));
    let host = match (named, hosts.as_slice()) {
        (Some(host), _) => host,
        (None, [only]) if only.segment.is_none() => only,
        (None, _) => return Behind::Unresolved,
    };
    let Some(map) = tree.ecam_iommu_map(host) else {
        return Behind::Nothing;
    };
    let Some((phandle, stream)) = map.translate(u32::from(function.requester_id())) else {
        return Behind::Nothing;
    };
    match tree.smmu_v3s().find(|smmu| smmu.phandle == Some(phandle)) {
        Some(smmu) => behind(units, smmu.region.address, stream),
        None => Behind::Unresolved,
    }
}

/// What discovery found.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Report {
    /// VT-d units.
    pub(crate) vtd: usize,
    /// `SMMUv3`s.
    pub(crate) smmu_v3: usize,
    /// PCI functions whose DMA a unit translates.
    pub(crate) behind: usize,
    /// PCI functions firmware puts behind no unit.
    pub(crate) bypassing: usize,
    /// PCI functions firmware describes in a way this cannot follow.
    pub(crate) unresolved: usize,
}

/// Find every unit, and place every PCI function among `nodes`.
pub(crate) fn discover(
    view: &BootView<'_>,
    nodes: &[Arc<DeviceNode>],
) -> (Report, Vec<Unit>, Vec<Placement>) {
    let units = units(view);
    let mut report = Report {
        vtd: units.iter().filter(|unit| unit.kind == Kind::VtD).count(),
        smmu_v3: units
            .iter()
            .filter(|unit| unit.kind == Kind::SmmuV3)
            .count(),
        ..Report::default()
    };
    let functions: Vec<Address> = nodes
        .iter()
        .filter_map(|node| match node.location() {
            Location::Pci(address) => Some(address),
            Location::VirtioMmio(_) => None,
        })
        .collect();

    let mut placements = Vec::new();
    let mut record = |function: Address, found: Behind| match found {
        Behind::Unit { unit, stream } => {
            report.behind += 1;
            placements.push(Placement {
                function,
                unit,
                stream,
            });
        }
        Behind::Nothing => report.bypassing += 1,
        Behind::Unresolved => report.unresolved += 1,
    };

    if let Ok(firmware) = acpi::Firmware::open(view) {
        let tables = firmware.acpi();
        let dmar = tables.dmar().ok();
        let iort = tables.iort().ok();
        for &function in &functions {
            let found = match (&dmar, &iort) {
                (Some(table), _) => place_dmar(table, &units, function),
                (None, Some(table)) => place_iort(table, &units, function),
                (None, None) => Behind::Nothing,
            };
            record(function, found);
        }
    } else if let Ok(tree) = fdt::open(view) {
        for &function in &functions {
            record(function, place_tree(&tree, &units, function));
        }
    } else {
        for &function in &functions {
            record(function, Behind::Nothing);
        }
    }
    (report, units, placements)
}

// ---------------------------------------------------------------------------
// Domains
// ---------------------------------------------------------------------------

/// The next domain's number, so a pin can be checked against the domain that
/// took it.
static NEXT_DOMAIN: AtomicU64 = AtomicU64::new(1);

/// Whether degraded trusted mode has been announced.
static DEGRADED: AtomicBool = AtomicBool::new(false);

/// Why a domain refused to pin or unpin.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DomainError {
    /// There was nothing to pin.
    Empty,
    /// A frame's address does not fit in a physical address.
    OutOfRange,
    /// The pin was taken by another domain.
    Foreign,
}

/// The memory one device's DMA may reach, and the addresses it reaches it by.
#[derive(Debug)]
pub(crate) struct Domain {
    /// This domain's number.
    id: u64,
    /// Pages pinned and not yet unpinned.
    pinned: AtomicU64,
}

/// Pages pinned into one domain, and each one's device address.
///
/// Goes back through [`Domain::unpin`] before its frames are freed. A pin that
/// cannot be given back is forgotten with its frames rather than dropped: the
/// device may still reach them.
#[derive(Debug)]
#[must_use = "unpin it, or leak its frames with it"]
pub(crate) struct Pinned {
    /// The domain that took it.
    domain: u64,
    /// Each page's device address, in the order its frame was given.
    addresses: Vec<u64>,
}

impl Pinned {
    /// Each page's device address, in the order its frame was given. Not
    /// necessarily contiguous.
    pub(crate) fn addresses(&self) -> &[u64] {
        &self.addresses
    }

    /// Never give the pin back: its pages stay reachable by the device for
    /// good, and whoever holds their frames must keep them too.
    pub(crate) fn leak(self) {
        let _ = core::mem::ManuallyDrop::new(self);
    }
}

impl Domain {
    /// A domain no unit translates.
    pub(crate) fn untranslated() -> Self {
        Domain {
            id: NEXT_DOMAIN.fetch_add(1, Ordering::Relaxed),
            pinned: AtomicU64::new(0),
        }
    }

    /// Whether a unit translates this domain's DMA, so a device can reach only
    /// what is pinned into it.
    pub(crate) const fn translated(&self) -> bool {
        false
    }

    /// Pages pinned and not yet unpinned.
    pub(crate) fn pinned_pages(&self) -> u64 {
        self.pinned.load(Ordering::Relaxed)
    }

    /// Make `frames` reachable by the device, writable when `flags` says so,
    /// and say at which device addresses.
    ///
    /// # Errors
    ///
    /// [`DomainError::Empty`] for no frames, [`DomainError::OutOfRange`] for a
    /// frame number no physical address can hold.
    pub(crate) fn pin(&self, frames: &[u64], flags: MapFlags) -> Result<Pinned, DomainError> {
        // Nothing enforces a read-only pin until a unit translates the domain.
        let _ = flags;
        if frames.is_empty() {
            return Err(DomainError::Empty);
        }
        let addresses = frames
            .iter()
            .map(|frame| frame.checked_mul(PAGE_SIZE).ok_or(DomainError::OutOfRange))
            .collect::<Result<Vec<u64>, DomainError>>()?;
        if !DEGRADED.swap(true, Ordering::Relaxed) {
            println!(
                "  iommu    degraded trusted mode: no IOMMU domain is programmed, so device \
                 DMA reaches all of memory"
            );
        }
        let _ = self
            .pinned
            .fetch_add(addresses.len() as u64, Ordering::Relaxed);
        Ok(Pinned {
            domain: self.id,
            addresses,
        })
    }

    /// Take `pinned`'s pages back out of the domain.
    ///
    /// On a translated domain the device can no longer reach them once this
    /// returns, and their frames may be freed. On an untranslated one it still
    /// can: nothing stands between the device and physical memory, so the
    /// frames may be freed only once the device is known to be quiet — reset,
    /// or never given the addresses — and are otherwise held for good.
    ///
    /// # Errors
    ///
    /// [`DomainError::Foreign`], handing the pin back, when another domain took
    /// it.
    pub(crate) fn unpin(&self, pinned: Pinned) -> Result<(), (DomainError, Pinned)> {
        if pinned.domain != self.id {
            return Err((DomainError::Foreign, pinned));
        }
        let _ = self
            .pinned
            .fetch_sub(pinned.addresses.len() as u64, Ordering::Relaxed);
        Ok(())
    }
}

/// What the domain check found.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DomainReport {
    /// Pages pinned and unpinned.
    pub(crate) pinned: u64,
    /// Requests refused, each exactly as the rule requires.
    pub(crate) refusals: usize,
}

/// Pin two frames through the first PCI node's domain, and require the node to
/// hand out one domain, the pin to give each frame an address, the domain to
/// count what it holds, and a pin to be refused by any domain but its own.
///
/// # Errors
///
/// The first thing that is not so. The frames are then kept out of the
/// allocator, since a pin that was not given back may still be reachable.
pub(crate) fn check_domains(nodes: &[Arc<DeviceNode>]) -> Result<DomainReport, &'static str> {
    let mut report = DomainReport::default();
    let Some(node) = nodes
        .iter()
        .find(|node| matches!(node.location(), Location::Pci(_)))
    else {
        return Ok(report);
    };
    let domain = node.domain();
    if !Arc::ptr_eq(&domain, &node.domain()) {
        return Err("a device node handed out two domains");
    }
    let Some(first) = mm::allocate_frames(0) else {
        return Err("no frame to pin");
    };
    let Some(second) = mm::allocate_frames(0) else {
        mm::deallocate_frames(first, 0);
        return Err("no frame to pin");
    };
    pin_and_unpin(&domain, [first, second], &mut report)?;
    mm::deallocate_frames(first, 0);
    mm::deallocate_frames(second, 0);
    Ok(report)
}

/// The body of [`check_domains`], once it has its frames.
fn pin_and_unpin(
    domain: &Domain,
    frames: [u64; 2],
    report: &mut DomainReport,
) -> Result<(), &'static str> {
    let before = domain.pinned_pages();
    let pinned = domain
        .pin(&frames, MapFlags::DMA)
        .map_err(|_| "a domain refused to pin two frames")?;
    let expected = frames.map(|frame| frame * PAGE_SIZE);
    let addressed = domain.translated() || pinned.addresses() == expected.as_slice();
    let counted = domain.pinned_pages() == before + 2;

    let Err((DomainError::Foreign, pinned)) = Domain::untranslated().unpin(pinned) else {
        return Err("a domain unpinned a pin another domain took");
    };
    report.refusals += 1;
    if !addressed || !counted {
        pinned.leak();
        return Err(if addressed {
            "a domain miscounted the pages pinned into it"
        } else {
            "an untranslated domain gave a device address other than the frame's"
        });
    }
    if !matches!(domain.pin(&[], MapFlags::DMA), Err(DomainError::Empty)) {
        pinned.leak();
        return Err("a domain pinned nothing");
    }
    report.refusals += 1;
    if domain.unpin(pinned).is_err() {
        return Err("a domain refused its own pin");
    }
    if domain.pinned_pages() != before {
        return Err("a domain still counted pages it had unpinned");
    }
    report.pinned += 2;
    Ok(())
}
