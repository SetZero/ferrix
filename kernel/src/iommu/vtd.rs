//! Intel VT-d in legacy mode: a remapping unit's root and context tables, and
//! the second-level tables of the domains built on it.
//!
//! Written against the specification and checked against QEMU's
//! `hw/i386/intel_iommu.c`, which the boot test runs. Legacy mode needs, and
//! this does:
//!
//! * a **root table**, one entry per bus, pointing at that bus's **context
//!   table**, one entry per device and function, which names a domain and the
//!   root of its second-level tables — `libs/paging`'s [`VtdSecondLevel`],
//!   three levels over 39 bits;
//! * **register-based invalidation** of the context cache and the IOTLB, after
//!   anything the unit may have cached changes;
//! * **translation on**, after which a function with no context entry reaches
//!   nothing at all.
//!
//! Queued invalidation, interrupt remapping and fault events are not used.
//! QEMU honours the register interface while queued invalidation is off, and
//! the kernel's MSI-X messages are compatibility format, which QEMU passes
//! through untouched until interrupt remapping is enabled.

use alloc::collections::{BTreeMap, BTreeSet};

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_paging::vtd::VtdSecondLevel;
use ferrix_paging::{MapError, MapFlags};
use ferrix_pci::Address;
use ferrix_sync::IrqSpinLock;

use super::Fault;
use crate::mmio::Mmio;
use crate::{arch, mm, timer, vmap};

/// Bytes of registers mapped: every register legacy mode uses is in the first
/// page.
const WINDOW: u64 = 0x1000;

/// Version register.
const VER: u64 = 0x00;
/// Capability register.
const CAP: u64 = 0x08;
/// Extended capability register.
const ECAP: u64 = 0x10;
/// Global command register.
const GCMD: u64 = 0x18;
/// Global status register.
const GSTS: u64 = 0x1C;
/// Root table address register.
const RTADDR: u64 = 0x20;
/// Context command register.
const CCMD: u64 = 0x28;
/// Fault status register.
const FSTS: u64 = 0x34;

/// GCMD: turn translation on; GSTS: it is on.
const TE: u32 = 1 << 31;
/// GCMD: take the root table pointer; GSTS: it is taken.
const SRTP: u32 = 1 << 30;
/// GSTS bits reporting a standing enable, which every GCMD write repeats so as
/// not to turn it off: translation, queued invalidation, interrupt remapping
/// and compatibility-format interrupts.
const STANDING: u32 = 1 << 31 | 1 << 26 | 1 << 25 | 1 << 23;

/// CAP: the unit needs its write buffer flushed after every change, which this
/// driver does not do.
const CAP_RWBF: u64 = 1 << 4;
/// CAP: caching mode, in which even an entry that was not present is cached
/// and must be invalidated once it is.
const CAP_CM: u64 = 1 << 7;
/// CAP: the `SAGAW` bit for three-level, 39-bit tables.
const CAP_SAGAW_39: u64 = 1 << 9;

/// CCMD: invalidate the context cache; reads back set until done.
const ICC: u64 = 1 << 63;
/// CCMD: one source ID, named in bits 31:16.
const CCMD_DEVICE: u64 = 3 << 61;
/// CCMD: every entry.
const CCMD_GLOBAL: u64 = 1 << 61;

/// IOTLB register: invalidate; reads back set until done.
const IVT: u64 = 1 << 63;
/// IOTLB register: every entry.
const IOTLB_GLOBAL: u64 = 1 << 60;
/// IOTLB register: one domain, named in bits 47:32.
const IOTLB_DOMAIN: u64 = 2 << 60;

/// FSTS: a fault was lost for want of a free record. Write one to clear.
const PFO: u32 = 1 << 0;
/// Fault recording register, high half: the record holds a fault.
const FRCD_F: u64 = 1 << 63;
/// Fault recording register, high half: the faulting access was a read.
const FRCD_READ: u64 = 1 << 62;

/// Root and context entries: present.
const PRESENT: u64 = 1;
/// Context entry, high half: the address width field for three levels.
const AW_39: u64 = 1;

/// How long a command may take before the unit is given up on.
const PATIENCE_NANOS: u64 = 100_000_000;

/// One remapping unit.
#[derive(Debug)]
pub(crate) struct Unit {
    /// Its registers.
    registers: Mmio,
    /// Physical address of the root table.
    root: u64,
    /// Offset of the first fault recording register.
    faults: u64,
    /// Offset of the IOTLB invalidation register.
    iotlb: u64,
    /// Whether the unit is in caching mode.
    caching: bool,
    /// How many domain identifiers the unit supports.
    identifiers: u32,
    /// The tables' bookkeeping.
    tables: IrqSpinLock<Tables, arch::Irq>,
    /// Held across a command and its wait, so two cannot interleave.
    commands: IrqSpinLock<(), arch::Irq>,
}

/// What a unit has handed out.
#[derive(Debug, Default)]
struct Tables {
    /// Each bus's context table, by physical address.
    contexts: BTreeMap<u8, u64>,
    /// Domain identifiers in use.
    identifiers: BTreeSet<u16>,
}

/// A domain on one unit: the function whose context entry names it, the
/// identifier it names, and the root of its second-level tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Attached {
    /// The function.
    function: Address,
    /// The domain identifier.
    identifier: u16,
    /// Physical address of the second-level root table.
    root: u64,
}

impl Attached {
    /// The source ID the unit sees the function as.
    pub(crate) fn stream(&self) -> u32 {
        u32::from(self.function.requester_id())
    }
}

impl Unit {
    /// Map the unit whose registers are at `phys`, and require it to be able to
    /// do what this driver asks.
    ///
    /// # Errors
    ///
    /// Why the unit is left alone.
    pub(crate) fn open(phys: u64) -> Result<Unit, &'static str> {
        let base =
            vmap::map_device(phys, WINDOW).map_err(|_| "its registers could not be mapped")?;
        let registers = Mmio::at(base);
        let refuse = |why: &'static str| -> Result<Unit, &'static str> {
            let _ = vmap::unmap_device(base);
            Err(why)
        };
        if (registers.read32(VER) >> 4) & 0xF != 1 {
            return refuse("it is not a version 1 unit");
        }
        let cap = read64(registers, CAP);
        let ecap = read64(registers, ECAP);
        if cap & CAP_SAGAW_39 == 0 {
            return refuse("it cannot walk three-level tables");
        }
        if cap & CAP_RWBF != 0 {
            return refuse("it needs its write buffer flushed");
        }
        if registers.read32(GSTS) & TE != 0 {
            return refuse("firmware left it translating");
        }
        let Some(root) = table() else {
            return refuse("no frame for its root table");
        };
        Ok(Unit {
            registers,
            root,
            faults: ((cap >> 24) & 0x3FF) * 16,
            iotlb: ((ecap >> 8) & 0x3FF) * 16 + 8,
            caching: cap & CAP_CM != 0,
            identifiers: 1 << (4 + 2 * (cap & 0b111)),
            tables: IrqSpinLock::new(Tables::default()),
            commands: IrqSpinLock::new(()),
        })
    }

    /// Point the unit at its root table, make it forget what it cached, and
    /// turn translation on. From here a function with no context entry reaches
    /// nothing.
    ///
    /// # Errors
    ///
    /// The command the unit never finished.
    pub(crate) fn enable(&self) -> Result<(), &'static str> {
        write64(self.registers, RTADDR, self.root);
        self.command(SRTP, "it never took its root table")?;
        self.invalidate_context(CCMD_GLOBAL)?;
        self.invalidate_iotlb(IOTLB_GLOBAL)?;
        // A fault firmware left recorded would otherwise be read as ours.
        if read64(self.registers, self.faults + 8) & FRCD_F != 0 {
            self.registers.write32(self.faults + 12, 1 << 31);
        }
        self.registers.write32(FSTS, PFO);
        self.command(TE, "it never started translating")
    }

    /// The fault the unit's first recording register holds, cleared so the unit
    /// can record the next, or `None`.
    ///
    /// QEMU's unit has one record, and drops a second fault from the same
    /// device while it is full, so a caller that wants a particular fault clears
    /// the record before provoking it.
    pub(crate) fn take_fault(&self) -> Option<Fault> {
        let high = read64(self.registers, self.faults + 8);
        if high & FRCD_F == 0 {
            return None;
        }
        let low = read64(self.registers, self.faults);
        self.registers.write32(self.faults + 12, 1 << 31);
        self.registers.write32(FSTS, PFO);
        Some(Fault {
            stream: (high & 0xFFFF) as u32,
            page: low & !0xFFF,
            write: high & FRCD_READ == 0,
        })
    }

    /// Give `function` a domain of its own on this unit: an empty second-level
    /// tree its context entry points at.
    ///
    /// # Errors
    ///
    /// Why it could not: no frames, no identifier left, the function already
    /// attached, or a unit that never finished invalidating.
    pub(crate) fn attach(&self, function: Address) -> Result<Attached, &'static str> {
        let root = table().ok_or("no frame for a domain's tables")?;
        let attached = match self.install(function, root) {
            Ok(attached) => attached,
            Err(why) => {
                mm::deallocate_frames(root / PAGE_SIZE, 0);
                return Err(why);
            }
        };
        // If this fails the entry stays, and so do the tables it points at.
        self.invalidate_context(CCMD_DEVICE | u64::from(function.requester_id()) << 16)?;
        Ok(attached)
    }

    /// Write `function`'s context entry to point at `root`.
    fn install(&self, function: Address, root: u64) -> Result<Attached, &'static str> {
        let mut tables = self.tables.lock();
        let bus = function.bus();
        let context = match tables.contexts.get(&bus) {
            Some(&context) => context,
            None => {
                let context = table().ok_or("no frame for a context table")?;
                let _ = tables.contexts.insert(bus, context);
                write_entry(self.root + u64::from(bus) * 16, context | PRESENT);
                context
            }
        };
        let entry = context + devfn(function) * 16;
        if read_entry(entry) & PRESENT != 0 {
            return Err("the function already has a domain");
        }
        let identifier = (1..self.identifiers)
            .filter_map(|candidate| u16::try_from(candidate).ok())
            .find(|candidate| !tables.identifiers.contains(candidate))
            .ok_or("the unit has no domain identifier left")?;
        let _ = tables.identifiers.insert(identifier);
        // The high half first, so the entry is never present with a stale
        // domain identifier or address width.
        write_entry(entry + 8, AW_39 | u64::from(identifier) << 8);
        write_entry(entry, root | PRESENT);
        Ok(Attached {
            function,
            identifier,
            root,
        })
    }

    /// Take `attached`'s context entry away, make the unit forget it, and give
    /// back its root table. Every page in it must have been unmapped, which
    /// also gave back every table below the root.
    ///
    /// # Errors
    ///
    /// Why not; the root table is then kept, since the unit may still walk it.
    pub(crate) fn detach(&self, attached: Attached) -> Result<(), &'static str> {
        let context = self
            .tables
            .lock()
            .contexts
            .get(&attached.function.bus())
            .copied()
            .ok_or("the function's bus has no context table")?;
        let entry = context + devfn(attached.function) * 16;
        write_entry(entry, 0);
        write_entry(entry + 8, 0);
        self.invalidate_context(CCMD_DEVICE | u64::from(attached.function.requester_id()) << 16)?;
        self.invalidate_iotlb(IOTLB_DOMAIN | u64::from(attached.identifier) << 32)?;
        let _ = self.tables.lock().identifiers.remove(&attached.identifier);
        mm::deallocate_frames(attached.root / PAGE_SIZE, 0);
        Ok(())
    }

    /// Map the page at `phys` at I/O address `iova` in `attached`'s tables.
    ///
    /// # Errors
    ///
    /// What the tables refused.
    pub(crate) fn map(
        &self,
        attached: &Attached,
        iova: u64,
        phys: u64,
        flags: MapFlags,
    ) -> Result<(), MapError> {
        mm::map_io::<VtdSecondLevel>(attached.root, iova, phys, flags)
    }

    /// Take the page at `iova` out of `attached`'s tables. The unit may still
    /// reach it until [`Unit::flush`] returns.
    ///
    /// # Errors
    ///
    /// What the tables refused.
    pub(crate) fn unmap(&self, attached: &Attached, iova: u64) -> Result<(), MapError> {
        mm::unmap_io::<VtdSecondLevel>(attached.root, iova)
    }

    /// Where `attached`'s tables send an access to `iova`, walked as the unit
    /// walks them.
    pub(crate) fn resolve(&self, attached: &Attached, iova: u64) -> Option<u64> {
        mm::translate_io::<VtdSecondLevel>(attached.root, iova)
    }

    /// Make the unit forget what it cached of `attached`'s tables: after an
    /// unmap always, and after a map only in caching mode, where not-present
    /// entries are cached too.
    ///
    /// # Errors
    ///
    /// A unit that never finished.
    pub(crate) fn flush(&self, attached: &Attached, after_map: bool) -> Result<(), &'static str> {
        if after_map && !self.caching {
            return Ok(());
        }
        self.invalidate_iotlb(IOTLB_DOMAIN | u64::from(attached.identifier) << 32)
    }

    /// Set `bit` in GCMD, keeping every standing enable, and wait for GSTS to
    /// report it.
    fn command(&self, bit: u32, why: &'static str) -> Result<(), &'static str> {
        let _held = self.commands.lock();
        let standing = self.registers.read32(GSTS) & STANDING;
        self.registers.write32(GCMD, standing | bit);
        self.wait(|| self.registers.read32(GSTS) & bit != 0, why)
    }

    /// Invalidate the context cache for `scope`, and wait until it has.
    fn invalidate_context(&self, scope: u64) -> Result<(), &'static str> {
        let _held = self.commands.lock();
        write64(self.registers, CCMD, ICC | scope);
        self.wait(
            || read64(self.registers, CCMD) & ICC == 0,
            "it never finished invalidating its context cache",
        )
    }

    /// Invalidate the IOTLB for `scope`, and wait until it has.
    fn invalidate_iotlb(&self, scope: u64) -> Result<(), &'static str> {
        let _held = self.commands.lock();
        write64(self.registers, self.iotlb, IVT | scope);
        self.wait(
            || read64(self.registers, self.iotlb) & IVT == 0,
            "it never finished invalidating its IOTLB",
        )
    }

    /// Wait for `ready`, up to [`PATIENCE_NANOS`].
    fn wait(&self, ready: impl Fn() -> bool, why: &'static str) -> Result<(), &'static str> {
        let deadline = timer::now_nanos().saturating_add(PATIENCE_NANOS);
        while !ready() {
            if timer::now_nanos() > deadline {
                return Err(why);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }
}

/// A function's index in its bus's context table.
fn devfn(function: Address) -> u64 {
    u64::from(function.device()) << 3 | u64::from(function.function())
}

/// A zeroed frame for a table, by physical address.
fn table() -> Option<u64> {
    let frame = mm::allocate_frames(0)?;
    mm::zero_frame(frame);
    Some(frame * PAGE_SIZE)
}

/// Read the table entry at physical address `at`.
fn read_entry(at: u64) -> u64 {
    // SAFETY: `at` is an entry inside a root or context table this unit took
    // from the frame allocator for itself and never gave back, at an offset
    // that is a multiple of eight within the page; the direct map covers every
    // frame of RAM. The unit reads the entry too, which is why the access is
    // volatile.
    unsafe { core::ptr::read_volatile(mm::direct_map(at) as *const u64) }
}

/// Write the table entry at physical address `at`.
fn write_entry(at: u64, value: u64) {
    // SAFETY: as `read_entry`: a whole, aligned eight-byte entry in a table
    // only this unit's code writes.
    unsafe { core::ptr::write_volatile(mm::direct_map(at) as *mut u64, value) };
}

/// Read a 64-bit register as two 32-bit halves, low first.
fn read64(registers: Mmio, at: u64) -> u64 {
    u64::from(registers.read32(at)) | u64::from(registers.read32(at + 4)) << 32
}

/// Write a 64-bit register as two 32-bit halves, low first: a command in the
/// high half takes effect when that half is written, with the low half already
/// in place.
fn write64(registers: Mmio, at: u64, value: u64) {
    registers.write32(at, value as u32);
    registers.write32(at + 4, (value >> 32) as u32);
}
