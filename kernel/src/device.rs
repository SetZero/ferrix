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
//! For the same reason an aperture overlapping memory the kernel uses is
//! withheld rather than minted. Today that is the framebuffer the loader
//! handed over, which on a PC is the display adapter's first BAR and is where
//! a panic is drawn.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use ferrix_bootinfo::{BootView, PAGE_SIZE};
use ferrix_fdt::{Trigger as TreeTrigger, VIRTIO_MMIO_COMPATIBLE};
use ferrix_pci::Address;
use ferrix_pci::bar::{Bar, Region};
use ferrix_pci::capability::MsiX;
use ferrix_pci::msix;
use ferrix_sync::Once;

use crate::{acpi, fdt};

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

/// Memory the kernel uses that no aperture may overlap.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reserved {
    /// The boot framebuffer, as `start..end`, or an empty range.
    framebuffer: (u64, u64),
}

impl Reserved {
    /// What the kernel owns on this machine.
    pub(crate) fn of(view: &BootView<'_>) -> Self {
        let framebuffer = view.raw().framebuffer;
        let range = if framebuffer.is_present() {
            (
                framebuffer.phys,
                framebuffer.phys.saturating_add(framebuffer.size),
            )
        } else {
            (0, 0)
        };
        Reserved { framebuffer: range }
    }

    /// Whether `aperture` overlaps anything reserved.
    const fn covers(self, aperture: Aperture) -> bool {
        let (start, end) = self.framebuffer;
        start < end && aperture.overlaps(start, end)
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
    /// Apertures not minted because the kernel uses that memory.
    withheld: usize,
    /// The pages of the device's MSI-X table and pending-bit array, as
    /// `(phys, len)`: memory the device has that no aperture may reach,
    /// because whoever writes the table chooses which interrupt it raises.
    interrupt_tables: Vec<(u64, u64)>,
}

impl DeviceNode {
    /// The node for a PCI function, from its sized BARs.
    ///
    /// Memory BARs become apertures; I/O BARs do not, because an `IoMapping`
    /// is memory. A BAR firmware left unassigned, at address zero, is not an
    /// aperture either. The pages holding the MSI-X table and pending-bit
    /// array are cut out of whichever BAR holds them and recorded instead, so
    /// a BAR that is all table becomes no aperture at all. PCI vectors wait
    /// for MSI-X to be programmed, so there are none yet.
    pub(crate) fn pci(
        address: Address,
        regions: &[Region],
        msix: Option<&MsiX>,
        reserved: Reserved,
    ) -> Self {
        let mut node = DeviceNode {
            location: Location::Pci(address),
            apertures: Vec::new(),
            vectors: Vec::new(),
            withheld: 0,
            interrupt_tables: Vec::new(),
        };
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
    fn mint(&mut self, phys: u64, len: u64, cacheable: bool, reserved: Reserved) {
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
/// controller would need that first.
fn tree_nodes(view: &BootView<'_>, reserved: Reserved) -> Vec<DeviceNode> {
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

    for found in tree.compatible_nodes(VIRTIO_MMIO_COMPATIBLE) {
        let Some(region) = found.reg().next() else {
            continue;
        };
        let mut node = DeviceNode {
            location: Location::VirtioMmio(region.address),
            apertures: Vec::new(),
            vectors: Vec::new(),
            withheld: 0,
            interrupt_tables: Vec::new(),
        };
        node.mint(region.address, region.size, false, reserved);
        if gic {
            node.vectors = found
                .gic_interrupts()
                .map(|interrupt| Vector {
                    number: interrupt.id,
                    trigger: interrupt.trigger.map(|trigger| match trigger {
                        TreeTrigger::EdgeRising | TreeTrigger::EdgeFalling => Trigger::Edge,
                        TreeTrigger::LevelHigh | TreeTrigger::LevelLow => Trigger::Level,
                    }),
                })
                .collect();
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
    /// Apertures withheld because the kernel uses the memory.
    pub(crate) withheld: usize,
    /// MSI-X table and pending-bit ranges withheld from apertures.
    pub(crate) msix_withheld: usize,
    /// Vectors minted.
    pub(crate) vectors: usize,
    /// Of those, edge-triggered.
    pub(crate) edge: usize,
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
/// tree's — after requiring each to hand out exactly what it has.
///
/// # Errors
///
/// The first node that mints an aperture or vector it should refuse, refuses
/// one it should mint, or holds an aperture overlapping reserved memory.
pub(crate) fn publish(view: &BootView<'_>, pci: Vec<DeviceNode>) -> Result<Report, Failure> {
    let reserved = Reserved::of(view);
    let mut nodes = pci;
    let tree = tree_nodes(view, reserved);
    let mut report = Report {
        tree: tree.len(),
        ..Report::default()
    };
    nodes.extend(tree);

    for node in &nodes {
        check_node(node, reserved, &mut report)?;
    }

    let published = DEVICES.call_once(|| nodes.into_iter().map(Arc::new).collect());
    report.nodes = published.len();
    Ok(report)
}

/// Require one node's tokens to follow the rule.
fn check_node(node: &DeviceNode, reserved: Reserved, report: &mut Report) -> Result<(), Failure> {
    let fail = |what| Failure {
        location: node.location(),
        what,
    };
    report.withheld += node.withheld;

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
