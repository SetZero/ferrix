//! The Generic Interrupt Controller, version 2, on both Arm architectures.
//!
//! The register interface is the same on a Cortex-A7 as on a Cortex-A72; what
//! differs is how the kernel learns where it is — the MADT on AArch64, the
//! device tree on ARMv7-A — so the caller finds the two register blocks and
//! this drives them.
//!
//! Two blocks, and the split matters. The **distributor** is machine-wide: it
//! decides which CPU an interrupt goes to and whether it is enabled at all. The
//! **CPU interface** is per-core: it is what the core reads to find out which
//! interrupt arrived and writes to say it is done.
//!
//! Three kinds of interrupt, distinguished only by number: 0..16 are
//! software-generated, 16..32 are *private* peripherals — each core has its
//! own, and the architected timer is one — and 32 upwards are shared.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ferrix_pci::msix;
use ferrix_sync::IrqSpinLock;

use crate::irq::Msi;
use crate::mmio::Mmio;

/// Distributor control.
const GICD_CTLR: u64 = 0x000;
/// Distributor type, with the interrupt count in its low five bits.
const GICD_TYPER: u64 = 0x004;
/// Set-enable, one bit per interrupt.
const GICD_ISENABLER: u64 = 0x100;
/// Clear-enable, one bit per interrupt.
const GICD_ICENABLER: u64 = 0x180;
/// Priority, one byte per interrupt.
const GICD_IPRIORITYR: u64 = 0x400;
/// Target processors, one byte per interrupt, a bit per CPU interface.
/// Banked for interrupts 0..32, where each core reads back its own bit.
const GICD_ITARGETSR: u64 = 0x800;
/// Configuration, two bits per interrupt: the upper bit makes it
/// edge-triggered rather than level-sensitive.
const GICD_ICFGR: u64 = 0xC00;
/// Bytes of register window the distributor occupies.
const GICD_WINDOW: u64 = 0x1000;

/// CPU interface control.
const GICC_CTLR: u64 = 0x000;
/// Priority mask: interrupts at a numerically higher priority are not
/// delivered. Zero — the reset value — means none are.
const GICC_PMR: u64 = 0x004;
/// Interrupt acknowledge. Reading it claims the interrupt.
const GICC_IAR: u64 = 0x00C;
/// End of interrupt. The value read from `GICC_IAR` goes back here.
const GICC_EOIR: u64 = 0x010;
/// Bytes of register window the CPU interface occupies.
const GICC_WINDOW: u64 = 0x2000;

/// Enable the controller.
const CTLR_ENABLE: u32 = 1 << 0;

/// Let every priority through.
const PMR_ALL: u32 = 0xFF;

/// A middle priority, which is every interrupt's priority until something
/// needs otherwise. Stage 14 is where these stop being all the same.
const DEFAULT_PRIORITY: u8 = 0xA0;

/// Interrupt identifiers from here up are not real interrupts: 1023 is
/// "spurious", and the rest are reserved. Reading one means the controller had
/// nothing to give, and it must not be acknowledged.
pub(crate) const FIRST_SPECIAL_ID: u32 = 1020;

/// The identifier field of `GICC_IAR`.
const IAR_ID_MASK: u32 = 0x3FF;

/// Distributor registers.
static DISTRIBUTOR: AtomicU64 = AtomicU64::new(0);

/// This core's CPU interface registers.
static CPU_INTERFACE: AtomicU64 = AtomicU64::new(0);

/// Held across every change to part of a distributor word: see [`rmw`].
///
/// A leaf. Nothing is taken while it is held, and it is taken with interrupts
/// masked because [`enable`] is reached from a system call on any core.
/// [`msi_allocate`]'s caller holds its device's `minted` lock, so the order
/// is that lock, then this one. [`disable`] never takes it: a clear-enable is
/// one store, and it runs in interrupt context under `irq`'s `BOUND`.
static DISTRIBUTOR_RMW: IrqSpinLock<(), crate::arch::Irq> = IrqSpinLock::new(());

/// Spins [`rmw`] makes between its read and its write, so that two cores
/// racing into it both read before either writes.
///
/// **Zero outside the boot check.** Only `check::concurrent_enables` sets it,
/// for its own rounds, and puts it back to zero: it is how that check makes
/// the interleaving F-50 was about happen every round rather than rarely.
static WIDEN_SPINS: AtomicU32 = AtomicU32::new(0);

/// Change part of the distributor word at `register`: read it, apply
/// `change`, write it back, under [`DISTRIBUTOR_RMW`].
///
/// **The only way anything changes part of a distributor word.** Priority
/// and target registers hold four lines' bytes a word and configuration
/// sixteen lines' bits, so an unlocked read-modify-write by one core can
/// write back, over another core's change to a neighbouring line, the value
/// it read before that change: a target byte lost is a shared interrupt
/// delivered nowhere, an edge bit lost is an MSI dropped (F-50). Linux holds
/// `gic_lock` for the same three. Every other distributor access is a whole
/// word or a write-one store (enable, clear-enable, the SGI register), or a
/// read at bring-up, or the boot check's.
fn rmw(gicd: Mmio, register: u64, change: impl FnOnce(u32) -> u32) {
    let _held = DISTRIBUTOR_RMW.lock();
    let value = gicd.read32(register);
    for _ in 0..WIDEN_SPINS.load(Ordering::Relaxed) {
        core::hint::spin_loop();
    }
    gicd.write32(register, change(value));
}

/// Turn a stored base address into a window.
fn window(slot: &AtomicU64) -> Mmio {
    match slot.load(Ordering::Relaxed) {
        0 => Mmio::unmapped(),
        base => Mmio::at(base),
    }
}

/// Map the two register blocks at the physical addresses the machine's
/// description gave, and bring the controller up.
///
/// # Safety
///
/// (DEVICE) Must be called once, on the boot CPU, after the vector table is installed
/// and while interrupts are masked: it leaves the controller able to deliver.
pub(crate) unsafe fn init(distributor: u64, cpu_interface: u64) -> Result<(), &'static str> {
    let gicd = crate::vmap::map_device(distributor, GICD_WINDOW)
        .map_err(|_| "could not map the GIC distributor")?;
    let gicc = crate::vmap::map_device(cpu_interface, GICC_WINDOW)
        .map_err(|_| "could not map the GIC CPU interface")?;
    DISTRIBUTOR.store(gicd, Ordering::Relaxed);
    CPU_INTERFACE.store(gicc, Ordering::Relaxed);

    configure(Mmio::at(gicd), Mmio::at(gicc));
    record_target(Mmio::at(gicd));
    Ok(())
}

/// Put the controller into a known state: everything off, then enabled.
fn configure(gicd: Mmio, gicc: Mmio) {
    // `GICD_TYPER`'s low five bits give the line count as N, meaning
    // 32 * (N + 1) identifiers.
    let lines = (u64::from(gicd.read32(GICD_TYPER) & 0b1_1111) + 1) * 32;

    gicd.write32(GICD_CTLR, 0);

    // Disable every line and give it a defined priority. What firmware left
    // enabled is firmware's business; from here it is ours, and an interrupt
    // nothing has registered for would otherwise arrive as soon as the CPU
    // unmasks.
    let words = lines.div_ceil(32);
    for word in 0..words {
        gicd.write32(GICD_ICENABLER + word * 4, u32::MAX);
    }
    for line in (0..lines).step_by(4) {
        gicd.write32(
            GICD_IPRIORITYR + line,
            u32::from_ne_bytes([DEFAULT_PRIORITY; 4]),
        );
    }

    gicd.write32(GICD_CTLR, CTLR_ENABLE);

    // The priority mask resets to zero, which blocks everything. This is the
    // single most common reason a freshly written GIC driver delivers no
    // interrupts at all.
    gicc.write32(GICC_PMR, PMR_ALL);
    gicc.write32(GICC_CTLR, CTLR_ENABLE);
}

/// Let interrupt `id` through to this core.
pub(crate) fn enable(id: u32) {
    let gicd = window(&DISTRIBUTOR);
    let word = u64::from(id / 32) * 4;
    let bit = 1u32 << (id % 32);

    // Priority is per interrupt and one byte wide, so this is a read-modify-
    // write of the word holding it rather than a plain store.
    rmw(gicd, GICD_IPRIORITYR + u64::from(id & !3), |word| {
        let mut priorities = word.to_ne_bytes();
        if let Some(slot) = priorities.get_mut((id % 4) as usize) {
            *slot = DEFAULT_PRIORITY;
        }
        u32::from_ne_bytes(priorities)
    });

    // A shared interrupt goes only to the CPU interfaces its target byte
    // names, and nothing in the architecture says what that byte resets to:
    // on QEMU's virt it is zero, which delivers the interrupt nowhere. EDK2
    // happens to route what it touches, U-Boot does not, so a shared line
    // that works on AArch64 can be silent on ARMv7-A for no reason the
    // interrupt itself shows. One with no target is given this core.
    if id >= PRIVATE_LINES as u32 {
        rmw(gicd, GICD_ITARGETSR + u64::from(id & !3), |word| {
            let mut targets = word.to_ne_bytes();
            if let Some(slot) = targets.get_mut((id % 4) as usize)
                && *slot == 0
            {
                *slot = this_cpu_target(gicd);
            }
            u32::from_ne_bytes(targets)
        });
    }

    gicd.write32(GICD_ISENABLER + word, bit);

    // A private interrupt's enable bit is this core's copy, so remember it:
    // every other core has to be told the same thing when it comes up. See
    // `init_this_cpu`.
    if id < PRIVATE_LINES as u32 {
        let _ = PRIVATE_ENABLED.fetch_or(bit, Ordering::Relaxed);
    }
}

/// This core's bit in the target registers.
///
/// The target bytes of interrupts 0..32 are banked, and each core reads back
/// only its own CPU interface's bit in them: the one place a GICv2 says which
/// interface a core is. The first word that answers is used, as Linux does;
/// a uniprocessor GIC, whose target registers read as zero and ignore writes,
/// gets bit 0, which is harmless there.
fn this_cpu_target(gicd: Mmio) -> u8 {
    (0..PRIVATE_LINES)
        .step_by(4)
        .map(|line| gicd.read32(GICD_ITARGETSR + line))
        .find(|word| *word != 0)
        .map_or(1, |word| {
            word.to_ne_bytes()
                .into_iter()
                .fold(0, |mask, byte| mask | byte)
        })
}

/// Stop delivering `id` until [`enable`] turns it back on.
///
/// What an `Interrupt` object does between a line firing and its driver
/// acknowledging it, so a device that keeps asserting its line cannot keep a
/// processor in the handler. A clear-enable write stops new deliveries only:
/// one already signalled to a CPU interface still arrives, and the handler
/// has to tolerate that.
pub(crate) fn disable(id: u32) {
    let gicd = window(&DISTRIBUTOR);
    let word = u64::from(id / 32) * 4;
    let bit = 1u32 << (id % 32);
    gicd.write32(GICD_ICENABLER + word, bit);

    // The private copy `enable` keeps for cores that come up later.
    if id < PRIVATE_LINES as u32 {
        let _ = PRIVATE_ENABLED.fetch_and(!bit, Ordering::Relaxed);
    }
}

/// Whether `id` is let through, read back from the distributor register
/// [`enable`] and [`disable`] write: for the boot check.
pub(crate) fn enabled_for_check(id: u32) -> bool {
    window(&DISTRIBUTOR).read32(GICD_ISENABLER + u64::from(id / 32) * 4) & 1 << (id % 32) != 0
}

/// The highest line nothing has enabled: a private peripheral interrupt, as
/// this core's banked registers read, or a shared one the distributor has.
/// For the boot check.
pub(crate) fn idle_line_for_check(private: bool) -> Option<u32> {
    let lines = (window(&DISTRIBUTOR).read32(GICD_TYPER) & 0b1_1111) + 1;
    let private_lines = PRIVATE_LINES as u32;
    let mut range = if private {
        16..private_lines
    } else {
        private_lines..(lines * 32).min(FIRST_SPECIAL_ID)
    };
    range.rfind(|&id| !enabled_for_check(id))
}

/// Claim the interrupt that arrived, or `None` if there was none.
///
/// The value returned by the controller is kept whole for [`complete`]: the
/// CPU identifier in its upper bits has to go back exactly as it came, and
/// masking it off here is a bug that only shows on a multiprocessor.
pub(crate) fn claim() -> Option<(u32, u32)> {
    let acknowledgement = window(&CPU_INTERFACE).read32(GICC_IAR);
    let id = acknowledgement & IAR_ID_MASK;
    if id >= FIRST_SPECIAL_ID {
        return None;
    }
    Some((id, acknowledgement))
}

/// Tell the controller the interrupt has been handled.
pub(crate) fn complete(acknowledgement: u32) {
    window(&CPU_INTERFACE).write32(GICC_EOIR, acknowledgement);
}

// ---------------------------------------------------------------------------
// More than one core
// ---------------------------------------------------------------------------

/// Interrupts 0..32 — software-generated and private peripheral — whose
/// distributor registers are banked, one copy per core.
const PRIVATE_LINES: u64 = 32;

/// Software-generated interrupt register: writing it sends one.
const GICD_SGIR: u64 = 0xF00;
/// `GICD_SGIR` target list filter: every core but the one writing.
const SGIR_ALL_BUT_SELF: u32 = 0b01 << 24;

/// The software-generated interrupt inter-processor interrupts arrive on.
pub(crate) const IPI_SGI: u32 = 1;

/// Every private interrupt [`enable`] has been asked for, as a bit per line.
///
/// The boot core enables the timer and the inter-processor interrupt before
/// any other core exists, and those enables land in *its* copy of the banked
/// registers. Recording them is what lets a core coming up later be given the
/// same set rather than a hard-coded guess at what it should be.
static PRIVATE_ENABLED: AtomicU32 = AtomicU32::new(0);

/// Bring up this core's side of the controller, on a core other than the one
/// that ran [`init`].
///
/// Three things are per core. The CPU interface, whose priority mask resets to
/// blocking everything — it is banked, one address reaching whichever core
/// reads it, so the window `init` mapped serves this one too. The
/// distributor's registers for interrupts 0..32, which are banked as well:
/// `init` set priorities for the boot core's copy, and this core's copy is
/// still at its reset value. And the enable bits for those same lines.
///
/// **That last one is not a detail.** Every private interrupt the boot core
/// turned on is off on this one until it is turned on here, and the timer is
/// a private interrupt. A core whose timer is masked in the controller runs,
/// takes inter-processor interrupts, and is never preempted — so whatever it
/// picks first it runs forever. Stage 5 found this the way it deserved to be
/// found: three of four cores each ran one spinner and never the other two,
/// with the run queue holding three tasks and the core reporting itself
/// busy.
pub(crate) fn init_this_cpu() {
    let gicd = window(&DISTRIBUTOR);
    for line in (0..PRIVATE_LINES).step_by(4) {
        gicd.write32(
            GICD_IPRIORITYR + line,
            u32::from_ne_bytes([DEFAULT_PRIORITY; 4]),
        );
    }

    let gicc = window(&CPU_INTERFACE);
    gicc.write32(GICC_PMR, PMR_ALL);
    gicc.write32(GICC_CTLR, CTLR_ENABLE);

    // Every private line the boot core enabled, enabled here too. `enable`
    // itself would do, but it would also write the shared priority byte again
    // and re-record what is already recorded; this is the banked half alone.
    let enabled = PRIVATE_ENABLED.load(Ordering::Relaxed);
    if enabled != 0 {
        gicd.write32(GICD_ISENABLER, enabled);
    }
    record_target(gicd);
}

/// Most CPU interfaces a GICv2 has: its target lists are a byte wide.
const MAX_INTERFACES: usize = 8;

/// A [`TARGETS`] slot that holds something.
const TARGET_RECORDED: u64 = 1 << 63;

/// Each core's CPU interface bit, by its hardware identifier: what an SGI's
/// target list names it by. A slot is `TARGET_RECORDED`, the identifier
/// shifted up eight, and the bit in the low byte; each core records its own as
/// it comes up ([`init_this_cpu`], and the boot core in [`init`]), and nothing
/// is ever taken out.
static TARGETS: [AtomicU64; MAX_INTERFACES] = [const { AtomicU64::new(0) }; MAX_INTERFACES];

/// Record this core's CPU interface bit under its hardware identifier.
///
/// Read from the banked target registers ([`this_cpu_target`]), which is the
/// one place a GICv2 says which interface a core is: a core's number and its
/// interface's need not agree. A slot is claimed with a compare-exchange, so
/// two cores coming up at once take two.
fn record_target(gicd: Mmio) {
    let id = crate::arch::hardware_id();
    let entry = TARGET_RECORDED | (id << 8) | u64::from(this_cpu_target(gicd));
    for slot in &TARGETS {
        let held = slot.load(Ordering::Acquire);
        if held == entry {
            return;
        }
        if held == 0
            && slot
                .compare_exchange(0, entry, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            return;
        }
    }
}

/// The CPU interface bit of the core whose hardware identifier is `id`, if
/// it has recorded one.
fn target_of(id: u64) -> Option<u8> {
    TARGETS.iter().find_map(|slot| {
        let entry = slot.load(Ordering::Acquire);
        (entry & TARGET_RECORDED != 0 && (entry & !TARGET_RECORDED) >> 8 == id)
            .then(|| (entry & 0xFF) as u8)
    })
}

/// Interrupt every core but this one on [`IPI_SGI`].
///
/// The caller orders its own stores first. The receiving core acts on memory
/// this one wrote, and the interrupt is a device write that an ordinary memory
/// barrier does not order against those stores — so the barrier is a `dsb`,
/// which is an instruction, and this driver is shared by two architectures
/// that each spell it for themselves.
pub(crate) fn send_sgi_to_others() {
    window(&DISTRIBUTOR).write32(GICD_SGIR, SGIR_ALL_BUT_SELF | IPI_SGI);
}

/// Interrupt the one core whose hardware identifier is `id` on [`IPI_SGI`],
/// through its CPU interface bit in `GICD_SGIR`'s target list (filter zero:
/// the list as given).
///
/// A whole-word store, like [`send_sgi_to_others`]: the SGI register is
/// write-only and holds no other line's state, so it needs none of
/// [`DISTRIBUTOR_RMW`]'s protection, which is for the words shared between
/// lines (F-50). The caller orders its stores first, as for the broadcast.
///
/// # Errors
///
/// When that core has recorded no interface bit: the caller broadcasts.
pub(crate) fn send_sgi_to(id: u64) -> Result<(), &'static str> {
    let target = target_of(id).ok_or("that core's CPU interface is not recorded")?;
    window(&DISTRIBUTOR).write32(GICD_SGIR, (u32::from(target) << 16) | IPI_SGI);
    Ok(())
}

// ---------------------------------------------------------------------------
// Message-signalled interrupts, through a GICv2m frame
// ---------------------------------------------------------------------------

/// SPIs this driver hands out from a frame, at most: one word of bitmap.
const MSI_SPIS: u32 = 64;

/// Bytes of a `GICv2m` frame's register window.
const V2M_WINDOW: u64 = 0x1000;

/// Physical address of the `GICv2m` frame, or zero when there is none.
static V2M_FRAME: AtomicU64 = AtomicU64::new(0);

/// The GIC identifier the frame's SPIs start at.
static V2M_FIRST: AtomicU32 = AtomicU32::new(0);

/// How many of them this driver hands out.
static V2M_COUNT: AtomicU32 = AtomicU32::new(0);

/// Which of them are allocated, one bit each.
static V2M_TAKEN: AtomicU64 = AtomicU64::new(0);

/// Make interrupt `id` edge-triggered.
///
/// A `GICv2m` frame raises an SPI by pulsing it, and a GICv2 latches a pulse as
/// pending only on an edge-triggered line: on a level-sensitive one the level
/// is gone before anything samples it, and the interrupt is lost without a
/// trace. Changed while the line is still disabled, as the architecture asks.
fn set_edge_triggered(id: u32) {
    let bit = 1_u32 << (2 * (id % 16) + 1);
    rmw(
        window(&DISTRIBUTOR),
        GICD_ICFGR + u64::from(id / 16) * 4,
        |value| value | bit,
    );
}

/// Record the `GICv2m` frame at `phys`. `spis` is `(first identifier, count)`
/// when firmware states the range, and otherwise it is read from the frame's
/// own `MSI_TYPER`.
///
/// # Errors
///
/// A frame that cannot be mapped, or whose range is not shared peripheral
/// interrupts. The machine still boots; it has no MSI vectors to hand out.
pub(crate) fn init_msi_frame(phys: u64, spis: Option<(u32, u32)>) -> Result<(), &'static str> {
    if phys == 0 {
        return Err("the GICv2m frame is at address zero");
    }
    let range = match spis {
        Some((first, count)) if first < 1024 && count < 1024 => {
            msix::gicv2m_spis((first << 16) | count)
        }
        Some(_) => None,
        None => {
            let frame = crate::vmap::map_device(phys, V2M_WINDOW)
                .map_err(|_| "could not map the GICv2m frame")?;
            let typer = Mmio::at(frame).read32(msix::GICV2M_TYPER);
            let _ = crate::vmap::unmap_device(frame);
            msix::gicv2m_spis(typer)
        }
    }
    .ok_or("the GICv2m frame's range is not shared peripheral interrupts")?;
    V2M_FIRST.store(range.first, Ordering::Relaxed);
    V2M_COUNT.store(range.count.min(MSI_SPIS), Ordering::Relaxed);
    V2M_FRAME.store(phys, Ordering::Relaxed);
    Ok(())
}

/// The physical address of the `GICv2m` frame's page, which a device's MSI
/// writes land in, if the machine has one.
///
/// An IOMMU that translates a device's MSI writes, as an `SMMUv3` does, must map
/// this page into every domain or the device's interrupts stop.
pub(crate) fn msi_doorbell() -> Option<u64> {
    let frame = V2M_FRAME.load(Ordering::Relaxed);
    (frame != 0).then_some(frame & !0xFFF)
}

/// Take an SPI from the `GICv2m` frame, make it edge-triggered, enable it, and
/// say what a device writes to raise it.
///
/// # Errors
///
/// No usable frame, or every SPI already taken.
pub(crate) fn msi_allocate() -> Result<Msi, &'static str> {
    let frame = V2M_FRAME.load(Ordering::Relaxed);
    if frame == 0 {
        return Err("the machine describes no usable GICv2m frame");
    }
    let count = V2M_COUNT.load(Ordering::Relaxed);
    let index = loop {
        let taken = V2M_TAKEN.load(Ordering::Relaxed);
        let free = taken.trailing_ones();
        if free >= count {
            return Err("every GICv2m SPI is allocated");
        }
        if V2M_TAKEN
            .compare_exchange(
                taken,
                taken | 1 << free,
                Ordering::SeqCst,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            break free;
        }
    };
    let id = V2M_FIRST.load(Ordering::Relaxed) + index;
    let message = msix::gicv2m_message(frame, id).ok_or("the GICv2m frame's address overflows")?;
    set_edge_triggered(id);
    enable(id);
    Ok(Msi {
        number: id,
        address: message.address,
        data: message.data,
    })
}

// The distributor's concurrent-change check: a child, so that it reaches this
// driver's registers and its widening hook, and a file of its own, so that it
// counts as the check it is.
pub(crate) mod check;
