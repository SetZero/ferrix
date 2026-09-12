//! Stage 10: device nodes, and the only way to name a device's registers or
//! interrupts.
//!
//! A driver in ring 3 gets an `IoMapping` for each of its device's apertures
//! "and nothing outside it", and an `Interrupt` for each of its vectors
//! (`docs/ARCHITECTURE.md` §7). Checking a driver's request against what the
//! device has is one comparison, and a comparison is a thing somebody can
//! forget to make. So the comparison is a type instead: an [`Aperture`] or a
//! [`Vector`] can only be constructed here, from what enumeration found, and
//! the objects a driver is handed take those rather than addresses and
//! numbers. A request for memory the device does not decode has no way to
//! become an `Aperture`, so it has no way to become a mapping.
//!
//! # Which devices get a node
//!
//! Every PCI function enumeration found, and every `virtio,mmio` node of a
//! device tree on a machine without ACPI tables. The device tree is an
//! allowlist rather than a walk of every node with a `reg`, because most of
//! those nodes are devices the kernel drives itself — the console, the
//! interrupt controller, the timer — and a node for one of them would let a
//! driver map the kernel's own registers.
//!
//! # What an aperture may not overlap
//!
//! A BAR's address is whatever the register holds, and a device tree's `reg`
//! is whatever somebody wrote; neither is proof the range is the device's. So
//! an aperture is withheld rather than minted when it overlaps anything
//! [`Reserved`] names — every region of the memory map except firmware's own
//! descriptions of device memory, every ECAM window, every device window the
//! kernel has mapped for itself, the device tree's console and the boot
//! framebuffer — or an aperture another node already holds. A BAR of a
//! function whose memory decoding is off is not an aperture at all: nothing
//! says firmware placed it.
//!
//! Vectors are screened the same way. A device tree node gets only shared
//! peripheral interrupts, none the kernel has registered a handler on, and
//! none another node already holds.

use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use ferrix_bootinfo::{BootView, MemKind, PAGE_SIZE};
use ferrix_fdt::{Trigger as TreeTrigger, VIRTIO_MMIO_COMPATIBLE};
use ferrix_pci::Address;
use ferrix_pci::bar::{Bar, Region};
use ferrix_pci::capability::MsiX;
use ferrix_pci::msix;
use ferrix_sync::Once;

use crate::{acpi, fdt, irq, vmap};

/// GIC interrupt identifiers below this are software-generated or private to
/// one core, and neither is a device's line.
const FIRST_SHARED_INTERRUPT: u32 = 32;

/// A range of device memory a driver may be given, and nothing else.
///
/// Constructible only in this module. `len` is not always a whole number of
/// pages — QEMU's virtio-mmio transports are 0x200 bytes each, packed into
/// shared pages — so a mapping must not round an aperture outward, or it maps
/// a neighbouring device's registers. [`Aperture::whole_pages`] says whether
/// a page mapping of it is exact.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Aperture {
    /// Physical address of the first byte.
    phys: u64,
    /// Bytes long; never zero, and never past the end of the address space.
    len: u64,
    /// Whether reads have no side effects, so it may be mapped cacheable.
    cacheable: bool,
}

impl Aperture {
    /// Physical address of the first byte.
    pub(crate) const fn phys(self) -> u64 {
        self.phys
    }

    /// Length in bytes.
    pub(crate) const fn len(self) -> u64 {
        self.len
    }

    /// Whether the range may be mapped cacheable.
    #[expect(dead_code, reason = "read by stage 9's IoMapping::new")]
    pub(crate) const fn cacheable(self) -> bool {
        self.cacheable
    }

    /// Whether the aperture starts on a page boundary and is a whole number
    /// of pages, so a page mapping covers it and nothing else.
    pub(crate) const fn whole_pages(self) -> bool {
        self.phys.is_multiple_of(PAGE_SIZE) && self.len.is_multiple_of(PAGE_SIZE)
    }

    /// One past the last byte. Cannot overflow: nothing mints an aperture
    /// that would.
    const fn end(self) -> u64 {
        self.phys + self.len
    }

    /// Whether this aperture and `start..end` share a byte.
    const fn overlaps(self, start: u64, end: u64) -> bool {
        self.phys < end && start < self.end()
    }
}

/// How an interrupt line signals.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Trigger {
    /// On a transition.
    Edge,
    /// For as long as the line is asserted.
    Level,
}

/// An interrupt a driver may be given, and nothing else.
///
/// Constructible only in this module.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Vector {
    /// The number `irq::register` takes: a GIC identifier on the Arm
    /// machines, a vector on x86-64.
    number: u32,
    /// How the line signals, where firmware said.
    trigger: Option<Trigger>,
}

impl Vector {
    /// The number `irq::register` takes.
    pub(crate) const fn number(self) -> u32 {
        self.number
    }

    /// How the line signals, where firmware said.
    pub(crate) const fn trigger(self) -> Option<Trigger> {
        self.trigger
    }
}

/// Where a device node came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Location {
    /// A PCI function.
    Pci(Address),
    /// A `virtio,mmio` device tree node, by the address of its registers.
    VirtioMmio(u64),
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Pci(address) => write!(f, "pci {address}"),
            Location::VirtioMmio(base) => write!(f, "virtio,mmio@{base:#x}"),
        }
    }
}

/// Physical memory no aperture may overlap, as `start..end` ranges.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reserved {
    /// The ranges, unsorted: there are a few dozen, and they are searched
    /// once per aperture at boot.
    ranges: Vec<(u64, u64)>,
}

impl Reserved {
    /// Everything the kernel owns on this machine, given the ECAM windows
    /// enumeration is about to read.
    ///
    /// Taken before enumeration maps anything, so the device windows it
    /// records are the controllers the kernel drives — the local and I/O
    /// APICs, the HPET, the GIC — and not the buses about to be walked, which
    /// `ecam` names whole.
    pub(crate) fn of(view: &BootView<'_>, ecam: &[(u64, u64)]) -> Self {
        let mut reserved = Reserved::default();
        let framebuffer = view.raw().framebuffer;
        if framebuffer.is_present() {
            reserved.add(framebuffer.phys, framebuffer.size);
        }
        // Firmware's own `MMIO` descriptions are left out: they are where it
        // says device memory is, and a BAR is device memory.
        for region in view.regions() {
            if region.kind != MemKind::Mmio {
                reserved.add(region.base, region.len);
            }
        }
        reserved.ranges.extend_from_slice(ecam);
        reserved.ranges.extend(vmap::device_windows());
        if acpi::Firmware::open(view).is_err()
            && let Ok(tree) = fdt::open(view)
            && let Some(console) = tree.console()
        {
            for region in console.reg() {
                reserved.add(region.address, region.size);
            }
        }
        reserved
    }

    /// Reserve `len` bytes at `start`.
    fn add(&mut self, start: u64, len: u64) {
        if len > 0 {
            self.ranges.push((start, start.saturating_add(len)));
        }
    }

    /// Whether `aperture` overlaps anything reserved.
    fn covers(&self, aperture: Aperture) -> bool {
        self.ranges
            .iter()
            .any(|&(start, end)| aperture.overlaps(start, end))
    }
}

/// A device a driver can be given, with exactly the apertures and vectors it
/// has.
#[derive(Debug)]
pub(crate) struct DeviceNode {
    /// Where it was found.
    location: Location,
    /// Its memory, in BAR or `reg` order.
    apertures: Vec<Aperture>,
    /// Its interrupts, in firmware's order.
    vectors: Vec<Vector>,
    /// Apertures not minted because the kernel or another device has that
    /// memory.
    withheld: usize,
    /// The pages of the device's MSI-X table and pending-bit array, as
    /// `(phys, len)`: memory the device has that no aperture may reach,
    /// because whoever writes the table chooses which interrupt it raises.
    interrupt_tables: Vec<(u64, u64)>,
    /// Whether memory BARs were left out because the function's memory
    /// decoding was off.
    undecoded: bool,
    /// Firmware's interrupts not minted as vectors.
    withheld_vectors: usize,
}

impl DeviceNode {
    /// A node with nothing in it yet.
    const fn empty(location: Location) -> Self {
        DeviceNode {
            location,
            apertures: Vec::new(),
            vectors: Vec::new(),
            withheld: 0,
            interrupt_tables: Vec::new(),
            undecoded: false,
            withheld_vectors: 0,
        }
    }

    /// The node for a PCI function, from its sized BARs.
    ///
    /// Memory BARs become apertures; I/O BARs do not, because an `IoMapping`
    /// is memory. A BAR firmware left unassigned, at address zero, is not an
    /// aperture either, and no BAR is when `decoding` says the function's
    /// memory decoding is off. The pages holding the MSI-X table and
    /// pending-bit array are cut out of whichever BAR holds them and recorded
    /// instead, so a BAR that is all table becomes no aperture at all. PCI
    /// vectors wait for MSI-X to be programmed, so there are none yet.
    pub(crate) fn pci(
        address: Address,
        regions: &[Region],
        msix: Option<&MsiX>,
        decoding: bool,
        reserved: &Reserved,
    ) -> Self {
        let mut node = DeviceNode::empty(Location::Pci(address));
        if !decoding {
            node.undecoded = regions
                .iter()
                .any(|region| matches!(region.bar, Bar::Memory { .. }));
            return node;
        }
        for region in regions {
            let Bar::Memory {
                address: base,
                prefetchable,
                ..
            } = region.bar
            else {
                continue;
            };
            if base == 0 {
                continue;
            }
            for &(offset, len) in msix::withheld(region, msix, PAGE_SIZE).as_slice() {
                if let Some(start) = base.checked_add(offset) {
                    node.interrupt_tables.push((start, len));
                }
            }
            for &(offset, len) in msix::mappable(region, msix, PAGE_SIZE).as_slice() {
                if let Some(start) = base.checked_add(offset) {
                    node.mint(start, len, prefetchable, reserved);
                }
            }
        }
        node
    }

    /// Add an aperture of `len` bytes at `phys`, unless it is empty, runs off
    /// the address space, or overlaps reserved memory.
    fn mint(&mut self, phys: u64, len: u64, cacheable: bool, reserved: &Reserved) {
        if phys == 0 || len == 0 || phys.checked_add(len).is_none() {
            return;
        }
        let aperture = Aperture {
            phys,
            len,
            cacheable,
        };
        if reserved.covers(aperture) {
            self.withheld += 1;
        } else {
            self.apertures.push(aperture);
        }
    }

    /// Where the device was found.
    pub(crate) const fn location(&self) -> Location {
        self.location
    }

    /// Every aperture the device has.
    pub(crate) fn apertures(&self) -> &[Aperture] {
        &self.apertures
    }

    /// Every vector the device has.
    pub(crate) fn vectors(&self) -> &[Vector] {
        &self.vectors
    }

    /// `len` bytes at `phys`, if they lie inside one of the device's
    /// apertures. The only way to make an [`Aperture`] after enumeration.
    pub(crate) fn aperture(&self, phys: u64, len: u64) -> Option<Aperture> {
        if len == 0 {
            return None;
        }
        let end = phys.checked_add(len)?;
        self.apertures
            .iter()
            .find(|whole| whole.phys <= phys && end <= whole.end())
            .map(|whole| Aperture {
                phys,
                len,
                cacheable: whole.cacheable,
            })
    }

    /// The device's vector at `index`, if it has one.
    pub(crate) fn vector(&self, index: usize) -> Option<Vector> {
        self.vectors.get(index).copied()
    }
}

/// Every `virtio,mmio` node in the device tree, on a machine without ACPI.
///
/// Its vectors are decoded only when the tree's interrupt controller takes
/// three-cell GIC specifiers, which is every tree Ferrix boots with; a node's
/// `interrupt-parent` is not followed, so a machine with a second interrupt
/// controller would need that first. A vector is taken only if it is a shared
/// peripheral interrupt that no kernel handler and no earlier node holds. The
/// first that fails ends the node's list, as a bad specifier does, because a
/// driver asks for its interrupts by position.
fn tree_nodes(view: &BootView<'_>, reserved: &Reserved) -> Vec<DeviceNode> {
    let mut nodes = Vec::new();
    if acpi::Firmware::open(view).is_ok() {
        return nodes;
    }
    let Ok(tree) = fdt::open(view) else {
        return nodes;
    };
    let gic = tree
        .interrupt_controller()
        .is_some_and(|controller| controller.interrupt_cells() == Some(3));
    let mut held = BTreeSet::new();

    for found in tree.compatible_nodes(VIRTIO_MMIO_COMPATIBLE) {
        let Some(region) = found.reg().next() else {
            continue;
        };
        let mut node = DeviceNode::empty(Location::VirtioMmio(region.address));
        node.mint(region.address, region.size, false, reserved);
        if gic {
            let mut interrupts = found.gic_interrupts();
            for interrupt in interrupts.by_ref() {
                let usable = interrupt.id >= FIRST_SHARED_INTERRUPT
                    && !irq::is_registered(interrupt.id)
                    && held.insert(interrupt.id);
                if !usable {
                    node.withheld_vectors += 1;
                    break;
                }
                node.vectors.push(Vector {
                    number: interrupt.id,
                    trigger: interrupt.trigger.map(|trigger| match trigger {
                        TreeTrigger::EdgeRising | TreeTrigger::EdgeFalling => Trigger::Edge,
                        TreeTrigger::LevelHigh | TreeTrigger::LevelLow => Trigger::Level,
                    }),
                });
            }
            node.withheld_vectors += interrupts.count();
        }
        nodes.push(node);
    }
    nodes
}

/// Every device node, once boot has published them.
static DEVICES: Once<Vec<Arc<DeviceNode>>> = Once::new();

/// Every device node, or none before boot publishes them.
pub(crate) fn devices() -> &'static [Arc<DeviceNode>] {
    DEVICES.get().map_or(&[], Vec::as_slice)
}

/// What publishing the nodes found.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Report {
    /// Nodes published.
    pub(crate) nodes: usize,
    /// Of those, device tree nodes.
    pub(crate) tree: usize,
    /// Apertures minted.
    pub(crate) apertures: usize,
    /// Of those, apertures a page mapping cannot cover exactly.
    pub(crate) partial_pages: usize,
    /// Apertures withheld because the kernel or another device has the
    /// memory.
    pub(crate) withheld: usize,
    /// MSI-X table and pending-bit ranges withheld from apertures.
    pub(crate) msix_withheld: usize,
    /// Functions whose BARs were left out because memory decoding was off.
    pub(crate) undecoded: usize,
    /// Vectors minted.
    pub(crate) vectors: usize,
    /// Of those, edge-triggered.
    pub(crate) edge: usize,
    /// Firmware's interrupts not minted as vectors.
    pub(crate) vectors_withheld: usize,
    /// Requests refused, each exactly as the rule requires.
    pub(crate) refusals: usize,
}

/// A device node that broke the rule its tokens exist to enforce.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Failure {
    /// The node.
    pub(crate) location: Location,
    /// What it did.
    pub(crate) what: &'static str,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.what)
    }
}

/// Publish every device node — the PCI ones enumeration built, and the device
/// tree's — after requiring each to hand out exactly what it has, and no two
/// to hand out the same memory or the same interrupt.
///
/// # Errors
///
/// The first node that mints an aperture or vector it should refuse, refuses
/// one it should mint, holds an aperture overlapping reserved memory, or
/// shares an aperture or vector with another node.
pub(crate) fn publish(
    view: &BootView<'_>,
    pci: Vec<DeviceNode>,
    reserved: &Reserved,
) -> Result<Report, Failure> {
    let mut nodes = pci;
    let tree = tree_nodes(view, reserved);
    let mut report = Report {
        tree: tree.len(),
        ..Report::default()
    };
    nodes.extend(tree);

    // Two nodes with overlapping apertures are two drivers for one set of
    // registers, so a later node loses what an earlier one already holds.
    let mut claimed: Vec<(u64, u64)> = Vec::new();
    for node in &mut nodes {
        let before = node.apertures.len();
        node.apertures.retain(|aperture| {
            let clash = claimed
                .iter()
                .any(|&(start, end)| aperture.overlaps(start, end));
            if !clash {
                claimed.push((aperture.phys, aperture.end()));
            }
            !clash
        });
        node.withheld += before - node.apertures.len();
    }

    for node in &nodes {
        check_node(node, reserved, &mut report)?;
    }
    check_exclusive(&nodes)?;

    let published = DEVICES.call_once(|| nodes.into_iter().map(Arc::new).collect());
    report.nodes = published.len();
    Ok(report)
}

/// Require no two apertures anywhere to overlap, no vector to be held twice,
/// and every device tree vector to be a shared peripheral interrupt.
///
/// A second pass over what `publish` built rather than a restatement of how it
/// built it: this sorts every aperture of every node and compares neighbours.
fn check_exclusive(nodes: &[DeviceNode]) -> Result<(), Failure> {
    let mut all: Vec<(u64, u64, Location)> = nodes
        .iter()
        .flat_map(|node| {
            node.apertures
                .iter()
                .map(move |aperture| (aperture.phys, aperture.end(), node.location))
        })
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
            if matches!(node.location, Location::VirtioMmio(_))
                && vector.number < FIRST_SHARED_INTERRUPT
            {
                return Err(Failure {
                    location: node.location,
                    what: "a vector is not a shared peripheral interrupt",
                });
            }
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
fn check_node(node: &DeviceNode, reserved: &Reserved, report: &mut Report) -> Result<(), Failure> {
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

    for (index, &vector) in node.vectors().iter().enumerate() {
        report.vectors += 1;
        if vector.trigger() == Some(Trigger::Edge) {
            report.edge += 1;
        }
        if node.vector(index) != Some(vector) {
            return Err(fail("a vector was not handed out as recorded"));
        }
    }
    if node.vector(node.vectors().len()).is_some() {
        return Err(fail("a vector past the end was handed out"));
    }
    report.refusals += 1;
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
