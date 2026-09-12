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
//! # A bus at a time
//!
//! An ECAM window is a megabyte per bus, and a machine describes 256 buses
//! whether or not it has more than one. Mapping all of it would take 256 MiB
//! of the kernel's address space — more than half of what a 32-bit kernel's
//! arena has — and half a megabyte of page tables, to reach a handful of
//! functions on bus zero. So [`Space`] maps a bus's megabyte the first time
//! the walk reads from it, and gives every window back when it is dropped.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt;

use ferrix_bootinfo::BootView;
use ferrix_pci::bar::{self, Region};
use ferrix_pci::capability::{Capabilities, ExtendedCapabilities};
use ferrix_pci::ecam::{BYTES_PER_BUS, Window};
use ferrix_pci::header::{CLASS_BRIDGE, Endpoint, HeaderKind, SUBCLASS_HOST_BRIDGE};
use ferrix_pci::virtio::{self as virtio_pci, TYPE_ENTROPY, Transport};
use ferrix_pci::walk::{Function, Walk};
use ferrix_pci::{Address, ConfigSpace, PciError};

mod virtio;

use crate::device::{DeviceNode, Reserved};
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

/// Every ECAM host the machine describes, and how many descriptions could not
/// be used.
fn hosts(view: &BootView<'_>) -> (Vec<Host>, usize, Source) {
    let mut hosts = Vec::new();
    let mut refused = 0;

    if let Ok(firmware) = acpi::Firmware::open(view) {
        if let Ok(mcfg) = firmware.acpi().mcfg() {
            for allocation in mcfg.entries() {
                let window =
                    Window::new(allocation.segment, allocation.start_bus, allocation.end_bus);
                match (window, allocation.window_base()) {
                    (Some(window), Some(phys)) => hosts.push(Host { window, phys }),
                    _ => refused += 1,
                }
            }
        }
        return (hosts, refused, Source::Mcfg);
    }

    if let Ok(tree) = fdt::open(view) {
        for host in tree.ecam_hosts() {
            match Window::new(host.segment, host.start_bus, host.end_bus) {
                Some(window) => hosts.push(Host {
                    window,
                    phys: host.window.address,
                }),
                None => refused += 1,
            }
        }
    }
    (hosts, refused, Source::DeviceTree)
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
    /// Descriptions that could not be turned into a window.
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
    /// Functions with a complete virtio PCI transport.
    pub(crate) virtio: usize,
    /// Bytes of entropy virtio-rng devices wrote into memory the kernel gave
    /// them.
    pub(crate) entropy_bytes: u32,
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
    /// A virtio device could not be brought up.
    Transport(ferrix_virtio::pci::TransportError),
    /// A virtio queue said something impossible.
    Queue(ferrix_virtio::QueueError),
    /// The entropy self-check failed.
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
            Failure::Transport(error) => write!(f, "virtio: {error}"),
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

impl From<ferrix_virtio::pci::TransportError> for Failure {
    fn from(error: ferrix_virtio::pci::TransportError) -> Self {
        Failure::Transport(error)
    }
}

impl From<ferrix_virtio::QueueError> for Failure {
    fn from(error: ferrix_virtio::QueueError) -> Self {
        Failure::Queue(error)
    }
}

/// Find every function, size its BARs, walk its capabilities, and build a
/// device node for each from what its BARs decode.
pub(crate) fn check(view: &BootView<'_>) -> Result<(Report, Vec<DeviceNode>), Failure> {
    let (hosts, refused, source) = hosts(view);
    let reserved = Reserved::of(view);
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
    };
    let mut nodes = Vec::new();
    for host in hosts {
        check_host(host, reserved, &mut report, &mut nodes)?;
    }
    Ok((report, nodes))
}

/// Walk one host and examine everything it reaches.
fn check_host(
    host: Host,
    reserved: Reserved,
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
        let regions = check_function(&mut space, function, report)?;
        nodes.push(DeviceNode::pci(function.address, &regions, reserved));
    }
    Ok(())
}

/// Size every BAR of one function and walk both its capability lists,
/// returning the BARs it decodes.
fn check_function(
    space: &mut Space,
    function: Function,
    report: &mut Report,
) -> Result<Vec<Region>, Failure> {
    let Function { address, identity } = function;
    report.functions += 1;
    if identity.class.base == CLASS_BRIDGE && identity.class.sub == SUBCLASS_HOST_BRIDGE {
        report.host_bridges += 1;
    }

    for capability in Capabilities::new(&*space, address) {
        let _ = capability?;
        report.capabilities += 1;
    }
    for capability in ExtendedCapabilities::new(&*space, address) {
        let _ = capability?;
        report.capabilities += 1;
    }

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

    if let Some(transport) = Transport::find(&*space, address)? {
        report.virtio += 1;
        let subsystem = if identity.kind == HeaderKind::Endpoint {
            Endpoint::read(&*space, address)?.subsystem
        } else {
            0
        };
        if virtio_pci::device_type(&identity, subsystem) == Some(TYPE_ENTROPY) {
            let written = virtio::entropy(space, address, &transport, &regions)?;
            report.entropy_bytes = report.entropy_bytes.saturating_add(written);
        }
    }
    Ok(regions)
}
