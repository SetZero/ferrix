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

use core::sync::atomic::{AtomicU64, Ordering};

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
const FIRST_SPECIAL_ID: u32 = 1020;

/// The identifier field of `GICC_IAR`.
const IAR_ID_MASK: u32 = 0x3FF;

/// Distributor registers.
static DISTRIBUTOR: AtomicU64 = AtomicU64::new(0);

/// This core's CPU interface registers.
static CPU_INTERFACE: AtomicU64 = AtomicU64::new(0);

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
/// Must be called once, on the boot CPU, after the vector table is installed
/// and while interrupts are masked: it leaves the controller able to deliver.
pub(crate) unsafe fn init(distributor: u64, cpu_interface: u64) -> Result<(), &'static str> {
    let gicd = crate::vmap::map_device(distributor, GICD_WINDOW)
        .map_err(|_| "could not map the GIC distributor")?;
    let gicc = crate::vmap::map_device(cpu_interface, GICC_WINDOW)
        .map_err(|_| "could not map the GIC CPU interface")?;
    DISTRIBUTOR.store(gicd, Ordering::Relaxed);
    CPU_INTERFACE.store(gicc, Ordering::Relaxed);

    configure(Mmio::at(gicd), Mmio::at(gicc));
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
    let register = GICD_IPRIORITYR + u64::from(id & !3);
    let mut priorities = gicd.read32(register).to_ne_bytes();
    if let Some(slot) = priorities.get_mut((id % 4) as usize) {
        *slot = DEFAULT_PRIORITY;
    }
    gicd.write32(register, u32::from_ne_bytes(priorities));

    gicd.write32(GICD_ISENABLER + word, bit);
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

/// Bring up this core's side of the controller, on a core other than the one
/// that ran [`init`].
///
/// Two things are per core. The CPU interface, whose priority mask resets to
/// blocking everything — it is banked, one address reaching whichever core
/// reads it, so the window `init` mapped serves this one too. And the
/// distributor's registers for interrupts 0..32, which are banked as well:
/// `init` set priorities for the boot core's copy, and this core's copy is
/// still at its reset value.
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

    // Its enable bit is banked too, so every core turns on its own.
    enable(IPI_SGI);
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
