//! Stage 10: finding PCI functions.
//!
//! The kernel enumerates buses and does not drive devices
//! (`docs/ARCHITECTURE.md` §7). This is the enumeration: where firmware said
//! configuration space is, a [`ConfigSpace`] over it, and the walk `libs/pci`
//! already proves on the host, run over real devices.
//!
//! # Where configuration space is
//!
//! On a machine with ACPI tables — x86-64, and AArch64 under EDK2 — the MCFG
//! says, and it is authoritative even if a device tree came too. Otherwise the
//! device tree's `pci-host-ecam-generic` nodes say. The two disagree about
//! what their address means — bus zero's for the MCFG, the first bus's for a
//! device tree — and both parsers hand over the first bus's, so nothing here
//! has to remember which it read.
//!
//! Two descriptions of the same buses would enumerate every function on them
//! twice — two device nodes, two drivers, one device — so a host whose segment
//! and buses overlap one already accepted is refused. A device tree host that
//! does not say which segment it is gets the lowest one no other host names,
//! as Linux does, rather than every such host sharing segment zero.
//!
//! # A bus at a time
//!
//! An ECAM window is a megabyte per bus, and a machine describes 256 buses
//! whether or not it has more than one. Mapping all of it would take 256 MiB
//! of the kernel's address space — more than half of what a 32-bit kernel's
//! arena has — and half a megabyte of page tables, to reach a handful of
//! functions on bus zero. So [`Space`] maps a bus's megabyte the first time
//! the walk reads from it, and gives every window back when it is dropped.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt;
use core::ops::RangeInclusive;

use ferrix_bootinfo::BootView;
use ferrix_pci::bar::{self, Region};
use ferrix_pci::capability::{
    self as pci_capability, Capabilities, Capability, ExtendedCapabilities, ID_MSIX, MsiX,
};
use ferrix_pci::ecam::{BYTES_PER_BUS, Window};
use ferrix_pci::header::{
    BusNumbers, CLASS_BRIDGE, COMMAND, COMMAND_MEMORY_SPACE, Endpoint, HeaderKind,
    SUBCLASS_HOST_BRIDGE,
};
use ferrix_pci::virtio::{self as virtio_pci, SharedMemory, TYPE_ENTROPY, TYPE_GPU, Transport};
use ferrix_pci::walk::{Function, Walk};
use ferrix_pci::{Address, ConfigSpace, PciError};

mod virtio;

use crate::device::{DeviceNode, Reserved, Seen};
use crate::mmio::Mmio;
use crate::vmap;
use crate::{acpi, fdt};

/// Which description a host came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Source {
    /// The ACPI MCFG table.
    Mcfg,
    /// A `pci-host-ecam-generic` device tree node.
    DeviceTree,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Mcfg => "MCFG",
            Source::DeviceTree => "device-tree",
        })
    }
}

/// An ECAM window firmware described.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Host {
    /// The functions it reaches.
    pub(crate) window: Window,
    /// The physical address of its first bus.
    pub(crate) phys: u64,
}

/// One description of a host, before it is given a segment and checked
/// against the others.
struct Described {
    /// The segment, if the description names one.
    segment: Option<u16>,
    /// The first bus.
    start_bus: u8,
    /// The last bus.
    end_bus: u8,
    /// The physical address of the first bus.
    phys: u64,
}

/// Whether two bus ranges share a bus.
fn buses_overlap(a: &RangeInclusive<u8>, b: &RangeInclusive<u8>) -> bool {
    a.start() <= b.end() && b.start() <= a.end()
}

/// Every ECAM host the machine describes, and how many descriptions could not
/// be used.
fn hosts(view: &BootView<'_>) -> (Vec<Host>, usize, Source) {
    let mut refused = 0;
    let mut described = Vec::new();

    let source = if let Ok(firmware) = acpi::Firmware::open(view) {
        if let Ok(mcfg) = firmware.acpi().mcfg() {
            for allocation in mcfg.entries() {
                match allocation.window_base() {
                    Some(phys) => described.push(Described {
                        segment: Some(allocation.segment),
                        start_bus: allocation.start_bus,
                        end_bus: allocation.end_bus,
                        phys,
                    }),
                    None => refused += 1,
                }
            }
        }
        Source::Mcfg
    } else {
        if let Ok(tree) = fdt::open(view) {
            for host in tree.ecam_hosts() {
                described.push(Described {
                    segment: host.segment,
                    start_bus: host.start_bus,
                    end_bus: host.end_bus,
                    phys: host.window.address,
                });
            }
        }
        Source::DeviceTree
    };

    let named: BTreeSet<u16> = described.iter().filter_map(|host| host.segment).collect();
    let mut hosts: Vec<Host> = Vec::new();
    for host in described {
        let segment = host.segment.or_else(|| {
            (0..=u16::MAX).find(|candidate| {
                !named.contains(candidate)
                    && !hosts
                        .iter()
                        .any(|taken| taken.window.segment() == *candidate)
            })
        });
        let Some(window) =
            segment.and_then(|segment| Window::new(segment, host.start_bus, host.end_bus))
        else {
            refused += 1;
            continue;
        };
        let overlaps = hosts.iter().any(|taken| {
            taken.window.segment() == window.segment()
                && buses_overlap(&taken.window.buses(), &window.buses())
        });
        if overlaps {
            refused += 1;
            continue;
        }
        hosts.push(Host {
            window,
            phys: host.phys,
        });
    }
    (hosts, refused, source)
}

/// One host's configuration space, mapped a bus at a time.
#[derive(Debug)]
struct Space {
    /// The window.
    host: Host,
    /// Every bus touched so far: where its megabyte is mapped, or `None` if
    /// mapping it failed, so a failure is not retried on every read.
    buses: RefCell<BTreeMap<u8, Option<u64>>>,
}

impl Space {
    /// Configuration space for `host`, with nothing mapped yet.
    const fn new(host: Host) -> Self {
        Space {
            host,
            buses: RefCell::new(BTreeMap::new()),
        }
    }

    /// The registers of `bus`, mapping them on first use.
    fn bus(&self, bus: u8) -> Option<Mmio> {
        let mut buses = self.buses.borrow_mut();
        if let Some(mapped) = buses.get(&bus) {
            return mapped.map(Mmio::at);
        }
        let index = bus.checked_sub(*self.host.window.buses().start())?;
        let mapped = u64::from(index)
            .checked_mul(BYTES_PER_BUS)
            .and_then(|offset| self.host.phys.checked_add(offset))
            .and_then(|phys| vmap::map_device(phys, BYTES_PER_BUS).ok());
        let _ = buses.insert(bus, mapped);
        mapped.map(Mmio::at)
    }

    /// Whether any bus the walk reached could not be mapped.
    fn unmapped(&self) -> Option<u8> {
        self.buses
            .borrow()
            .iter()
            .find_map(|(bus, mapped)| mapped.is_none().then_some(*bus))
    }

    /// The window and offset of `width` bytes at `offset` in `function`'s
    /// space, or `None` if the access is outside the window, not aligned to
    /// its width, or on a bus that could not be mapped.
    ///
    /// The alignment is refused rather than served because a volatile read of
    /// a misaligned `u16` or `u32` is undefined behaviour; `libs/pci` never
    /// asks for one, so a refusal reads as all ones and nothing else changes.
    fn register(&self, function: Address, offset: u16, width: u16) -> Option<(Mmio, u64)> {
        if !offset.is_multiple_of(width) {
            return None;
        }
        let at = self.host.window.offset(function, offset, width)?;
        let registers = self.bus(function.bus())?;
        Some((registers, at % BYTES_PER_BUS))
    }
}

impl ConfigSpace for Space {
    fn read8(&self, function: Address, offset: u16) -> u8 {
        self.register(function, offset, 1)
            .map_or(u8::MAX, |(registers, at)| registers.read8(at))
    }

    fn read16(&self, function: Address, offset: u16) -> u16 {
        self.register(function, offset, 2)
            .map_or(u16::MAX, |(registers, at)| registers.read16(at))
    }

    fn read32(&self, function: Address, offset: u16) -> u32 {
        self.register(function, offset, 4)
            .map_or(u32::MAX, |(registers, at)| registers.read32(at))
    }

    fn write16(&mut self, function: Address, offset: u16, value: u16) {
        if let Some((registers, at)) = self.register(function, offset, 2) {
            registers.write16(at, value);
        }
    }

    fn write32(&mut self, function: Address, offset: u16, value: u32) {
        if let Some((registers, at)) = self.register(function, offset, 4) {
            registers.write32(at, value);
        }
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        for mapped in self.buses.get_mut().values().flatten() {
            let _ = vmap::unmap_device(*mapped);
        }
    }
}

/// What enumeration found.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Report {
    /// ECAM hosts described and walked.
    pub(crate) hosts: usize,
    /// Descriptions that could not be turned into a window, or that overlap
    /// one already accepted.
    pub(crate) refused: usize,
    /// Where the hosts were described.
    pub(crate) source: Source,
    /// Functions found.
    pub(crate) functions: usize,
    /// Of those, host bridges.
    pub(crate) host_bridges: usize,
    /// Bridges whose buses the walk did not follow.
    pub(crate) unfollowed: usize,
    /// BARs sized.
    pub(crate) bars: usize,
    /// Bytes of aperture those BARs decode.
    pub(crate) aperture_bytes: u64,
    /// Standard and extended capabilities walked.
    pub(crate) capabilities: usize,
    /// Virtio functions with a complete transport inside their memory BARs.
    pub(crate) virtio: usize,
    /// Bytes of entropy virtio-rng devices wrote into memory the kernel gave
    /// them.
    pub(crate) entropy_bytes: u32,
    /// Entropy checks skipped because the device refused or stalled.
    pub(crate) entropy_skipped: usize,
    /// Why the last one was skipped.
    pub(crate) entropy_skip: Option<&'static str>,
    /// Entropy requests whose completion arrived by MSI-X.
    pub(crate) entropy_by_interrupt: usize,
    /// Why the last request that completed without MSI-X was polled instead.
    pub(crate) entropy_polled: Option<&'static str>,
    /// Writes outside a device's translated domain that its unit faulted.
    pub(crate) out_of_domain_faulted: usize,
    /// The completion the device reported for the last faulted write anyway,
    /// if it did: a fact about the device model, printed so a boot log shows
    /// it (`virtio::Faulted`).
    pub(crate) out_of_domain_completed: Option<virtio::Completed>,
    /// Why the last out-of-domain write was not shown to fault, if one was not.
    pub(crate) out_of_domain_skip: Option<&'static str>,
}

/// Why enumeration failed.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure {
    /// A host's window was described but nothing answered on its root bus.
    NothingAnswered {
        /// The physical address the window was mapped from.
        phys: u64,
    },
    /// A bus the walk reached could not be mapped.
    Unmapped {
        /// The physical address of the host's window.
        phys: u64,
        /// The bus.
        bus: u8,
    },
    /// `libs/pci` refused what a function presented.
    Refused(PciError),
    /// A virtio queue said something impossible about a request in flight.
    Queue(ferrix_virtio::QueueError),
    /// The entropy self-check saw a completion that cannot be right.
    Entropy(&'static str),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Failure::NothingAnswered { phys } => {
                write!(f, "no function answered in the ECAM window at {phys:#x}")
            }
            Failure::Unmapped { phys, bus } => {
                write!(
                    f,
                    "bus {bus:#04x} of the ECAM window at {phys:#x} could not be mapped"
                )
            }
            Failure::Refused(error) => write!(f, "{error}"),
            Failure::Queue(error) => write!(f, "virtio queue: {error:?}"),
            Failure::Entropy(what) => write!(f, "virtio-rng: {what}"),
        }
    }
}

impl From<PciError> for Failure {
    fn from(error: PciError) -> Self {
        Failure::Refused(error)
    }
}

impl From<ferrix_virtio::QueueError> for Failure {
    fn from(error: ferrix_virtio::QueueError) -> Self {
        Failure::Queue(error)
    }
}

/// Find every function, size its BARs, walk its capabilities, and build a
/// device node for each from what its BARs decode.
///
/// Returns what apertures may not overlap too, which publishing the device
/// tree's nodes needs as well.
pub(crate) fn check(view: &BootView<'_>) -> Result<(Report, Vec<DeviceNode>, Reserved), Failure> {
    let (hosts, refused, source) = hosts(view);
    let ecam: Vec<(u64, u64)> = hosts
        .iter()
        .map(|host| (host.phys, host.phys.saturating_add(host.window.len())))
        .collect();
    let reserved = Reserved::of(view, &ecam);
    let mut report = Report {
        hosts: hosts.len(),
        refused,
        source,
        functions: 0,
        host_bridges: 0,
        unfollowed: 0,
        bars: 0,
        aperture_bytes: 0,
        capabilities: 0,
        virtio: 0,
        entropy_bytes: 0,
        entropy_skipped: 0,
        entropy_skip: None,
        entropy_by_interrupt: 0,
        entropy_polled: None,
        out_of_domain_faulted: 0,
        out_of_domain_completed: None,
        out_of_domain_skip: None,
    };
    let mut nodes = Vec::new();
    for host in hosts {
        check_host(host, &reserved, &mut report, &mut nodes)?;
    }
    Ok((report, nodes, reserved))
}

/// Walk one host and examine everything it reaches.
fn check_host(
    host: Host,
    reserved: &Reserved,
    report: &mut Report,
    nodes: &mut Vec<DeviceNode>,
) -> Result<(), Failure> {
    let mut space = Space::new(host);

    // Collected first: the walk borrows the space, and sizing writes to it.
    let mut found: Vec<Function> = Vec::new();
    for item in Walk::new(&space, host.window.segment(), host.window.buses()) {
        match item {
            Ok(function) => found.push(function),
            Err(_) => report.unfollowed += 1,
        }
    }
    if let Some(bus) = space.unmapped() {
        return Err(Failure::Unmapped {
            phys: host.phys,
            bus,
        });
    }
    if found.is_empty() {
        return Err(Failure::NothingAnswered { phys: host.phys });
    }

    for function in found {
        let (regions, msix, decoding, transport, host_visible) =
            check_function(&mut space, function, reserved, report)?;
        // Where the function's configuration space is, which minting one of
        // its MSI-X vectors writes to turn MSI-X on.
        let config_phys = host
            .window
            .offset(function.address, 0, 1)
            .and_then(|offset| host.phys.checked_add(offset));
        // What sysfs shows beside the identity: the board's ids for an
        // endpoint, the bus behind a bridge.
        let subsystem = Endpoint::read(&space, function.address).map_or((0, 0), |endpoint| {
            (endpoint.subsystem_vendor, endpoint.subsystem)
        });
        let secondary_bus = BusNumbers::read(&space, function.address)
            .ok()
            .map(|numbers| numbers.secondary);
        nodes.push(DeviceNode::pci(
            function.address,
            &Seen {
                config_phys,
                identity: &function.identity,
                transport: transport.as_ref(),
                subsystem,
                secondary_bus,
                host_visible: host_visible.as_ref(),
            },
            &regions,
            msix.as_ref(),
            decoding,
            reserved,
        ));
    }
    Ok(())
}

/// What examining one function yields: its sized BARs, its MSI-X capability
/// if it has one, whether firmware left its memory decoding on, its virtio
/// transport, and a virtio GPU's host-visible window.
type Examined = (
    Vec<Region>,
    Option<(Capability, MsiX)>,
    bool,
    Option<Transport>,
    Option<SharedMemory>,
);

/// Size every BAR of one function and walk both its capability lists,
/// returning the BARs it decodes, its MSI-X capability if it has one, and
/// whether firmware left its memory decoding on.
fn check_function(
    space: &mut Space,
    function: Function,
    reserved: &Reserved,
    report: &mut Report,
) -> Result<Examined, Failure> {
    let Function { address, identity } = function;
    report.functions += 1;
    if identity.class.base == CLASS_BRIDGE && identity.class.sub == SUBCLASS_HOST_BRIDGE {
        report.host_bridges += 1;
    }
    // Read before sizing, which switches decoding off and back to this.
    let decoding = space.read16(address, COMMAND) & COMMAND_MEMORY_SPACE != 0;

    for capability in Capabilities::new(&*space, address) {
        let _ = capability?;
        report.capabilities += 1;
    }
    for capability in ExtendedCapabilities::new(&*space, address) {
        let _ = capability?;
        report.capabilities += 1;
    }

    let msix = match pci_capability::find(&*space, address, ID_MSIX)? {
        Some(capability) => Some((capability, MsiX::read(&*space, capability)?)),
        None => None,
    };

    let mut regions = Vec::new();
    for index in 0..identity.kind.bar_slots() {
        match bar::size(space, address, identity.kind, index) {
            Ok(Some(region)) => {
                report.bars += 1;
                report.aperture_bytes = report.aperture_bytes.saturating_add(region.size);
                regions.push(region);
            }
            // An unimplemented slot, or the upper half of a 64-bit BAR.
            Ok(None) | Err(PciError::NoSuchBar { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }

    // Only a virtio device's vendor capabilities are virtio's: an Intel
    // bridge carries one of its own, shorter than virtio's format, and
    // reading it as virtio's would refuse a perfectly ordinary chipset.
    let subsystem = if identity.kind == HeaderKind::Endpoint {
        Endpoint::read(&*space, address)?.subsystem
    } else {
        0
    };
    let mut found_transport = None;
    let mut host_visible = None;
    if let Some(kind) = virtio_pci::device_type(&identity, subsystem)
        && let Some(transport) = Transport::find(&*space, address)?
    {
        transport.verify(&regions)?;
        report.virtio += 1;
        found_transport = Some(transport);
        // The window a virtio GPU's host maps blob resources into
        // (`docs/GPU.md` §6.1). Taken only whole inside a BAR: the render
        // core maps its pages into programs, and a window that ran past its
        // BAR would hand them whatever lies beyond.
        if kind == TYPE_GPU {
            host_visible =
                SharedMemory::find(&*space, address, ferrix_virtio::gpu::SHM_ID_HOST_VISIBLE)?
                    .filter(|window| regions.iter().any(|region| window.fits(region)));
        }
        if kind == TYPE_ENTROPY {
            match virtio::entropy(
                space,
                address,
                &transport,
                &regions,
                msix.as_ref(),
                reserved,
            )? {
                virtio::Entropy::Read {
                    bytes,
                    polled,
                    out_of_domain,
                } => {
                    report.entropy_bytes = report.entropy_bytes.saturating_add(bytes);
                    match polled {
                        None => report.entropy_by_interrupt += 1,
                        Some(why) => report.entropy_polled = Some(why),
                    }
                    match out_of_domain {
                        Some(Ok(faulted)) => {
                            report.out_of_domain_faulted += 1;
                            report.out_of_domain_completed =
                                faulted.completed.or(report.out_of_domain_completed);
                        }
                        Some(Err(why)) => report.out_of_domain_skip = Some(why),
                        None => {}
                    }
                }
                virtio::Entropy::Skipped(why) => {
                    report.entropy_skipped += 1;
                    report.entropy_skip = Some(why);
                }
            }
        }
    }
    Ok((regions, msix, decoding, found_transport, host_visible))
}
