//! The Generic Interrupt Controller, version 3.
//!
//! What changed from `crate::arch::gicv2`, and why this is a second driver
//! rather than a flag on the first:
//!
//! * The **CPU interface is system registers** (`ICC_*_EL1`), not a memory
//!   window. Claiming, retiring and sending an inter-processor interrupt are
//!   each one instruction, in `super::cpu`.
//! * Each core has a **redistributor**: a pair of 64 KiB frames holding what a
//!   GICv2 banked in the distributor for interrupts 0..32 -- their enables and
//!   priorities -- plus a power control a core must clear before its
//!   interface will take anything. The frames sit one after another in one
//!   region, and each says which core it belongs to.
//! * A shared interrupt is routed by **affinity** (`GICD_IROUTER`, the target
//!   core's `MPIDR`), not by a CPU interface bit mask.
//!
//! The distributor's enable, priority and configuration registers are where a
//! GICv2's are, so for interrupts 32 upwards the two drivers write the same
//! offsets.
//!
//! No message-signalled interrupts yet: a GICv3 takes them through an ITS,
//! which is its own driver, and until there is one a device asking for a
//! vector is told there are none.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::cpu;
use crate::mmio::Mmio;

/// Distributor control.
const GICD_CTLR: u64 = 0x0000;
/// Distributor type, with the interrupt count in its low five bits.
const GICD_TYPER: u64 = 0x0004;
/// Group, one bit per interrupt: set is group 1, the non-secure kernel's.
const GICD_IGROUPR: u64 = 0x0080;
/// Set-enable, one bit per interrupt.
const GICD_ISENABLER: u64 = 0x0100;
/// Clear-enable, one bit per interrupt.
const GICD_ICENABLER: u64 = 0x0180;
/// Priority, one byte per interrupt.
const GICD_IPRIORITYR: u64 = 0x0400;
/// Routing, eight bytes per shared interrupt: the target core's affinity.
const GICD_IROUTER: u64 = 0x6000;
/// Bytes of register window the distributor occupies.
const GICD_WINDOW: u64 = 0x1_0000;

/// `GICD_CTLR`: a register write is still taking effect.
const GICD_CTLR_RWP: u32 = 1 << 31;
/// `GICD_CTLR`: affinity routing, which is what makes this a GICv3 rather
/// than one emulating a GICv2. Bit 4 in both the secure and non-secure views.
const GICD_CTLR_ARE: u32 = 1 << 4;
/// `GICD_CTLR`: enable group 1. Bits 0 and 1 are "group 1" and "group 1A" to
/// a non-secure writer, "group 0" and "group 1" on a GIC with one security
/// state; setting both is right under either.
const GICD_CTLR_ENABLE_GROUP1: u32 = 0b11;

/// Redistributor control: bit 3 is its register-write-pending bit.
const GICR_CTLR: u64 = 0x0000;
/// Redistributor type, 64 bits: the owning core's affinity in the upper half.
const GICR_TYPER: u64 = 0x0008;
/// Redistributor power control.
const GICR_WAKER: u64 = 0x0014;
/// The second frame, which holds interrupts 0..32's own registers.
const GICR_SGI_FRAME: u64 = 0x1_0000;
/// Bytes a GICv3 redistributor spans: the control frame and the SGI frame.
const GICR_STRIDE: u64 = 0x2_0000;
/// Bytes a `GICv4` one spans, with two more frames for virtual LPIs.
const GICR_STRIDE_V4: u64 = 0x4_0000;

/// `GICR_CTLR`: a register write is still taking effect.
const GICR_CTLR_RWP: u32 = 1 << 3;
/// `GICR_TYPER`: this is the last redistributor in the region.
const GICR_TYPER_LAST: u32 = 1 << 4;
/// `GICR_TYPER`: the redistributor has virtual LPI frames, and so a `GICv4`'s
/// stride.
const GICR_TYPER_VLPIS: u32 = 1 << 1;
/// `GICR_WAKER`: the core is asleep as far as the controller is concerned.
const WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
/// `GICR_WAKER`: and the redistributor has finished going to sleep.
const WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

/// Register-write-pending and wake-up polls give up after this many reads. A
/// real controller finishes in a handful; one that never does is described
/// wrongly, and should be said to be rather than hung on.
const POLL_LIMIT: u32 = 1_000_000;

/// A middle priority, as the GICv2 driver gives every interrupt.
const DEFAULT_PRIORITY: u8 = 0xA0;

/// Interrupt identifiers 1020..1024 are not interrupts: 1023 is "spurious".
pub(crate) const FIRST_SPECIAL_ID: u32 = 1020;

/// The software-generated interrupt inter-processor interrupts arrive on,
/// the same number the GICv2 driver uses.
pub(crate) const IPI_SGI: u32 = 1;

/// `ICC_SGI1R_EL1`: send to every core but this one.
const SGI1R_ALL_BUT_SELF: u64 = 1 << 40;
/// `ICC_SGI1R_EL1`: where the interrupt's identifier goes.
const SGI1R_INTID_SHIFT: u32 = 24;

/// The identifier field of `ICC_IAR1_EL1`.
const IAR_ID_MASK: u64 = 0xFF_FFFF;

/// Interrupts 0..32, which each core has its own copy of in its redistributor.
const PRIVATE_LINES: u32 = 32;

/// The affinity fields of `MPIDR_EL1`, which are also `GICD_IROUTER`'s.
const MPIDR_AFFINITY: u64 = 0x0000_00FF_00FF_FFFF;

/// Distributor registers, mapped.
static DISTRIBUTOR: AtomicU64 = AtomicU64::new(0);
/// The redistributor region, mapped, and its length.
static REDISTRIBUTORS: AtomicU64 = AtomicU64::new(0);
/// Bytes of it.
static REDISTRIBUTORS_LEN: AtomicU64 = AtomicU64::new(0);

/// Every private interrupt [`enable`] has been asked for, one bit per line,
/// for the same reason the GICv2 driver keeps it: a core that comes up later
/// has to be given the boot core's set.
static PRIVATE_ENABLED: AtomicU32 = AtomicU32::new(0);

/// Turn a stored base address into a window.
fn window(slot: &AtomicU64) -> Mmio {
    match slot.load(Ordering::Relaxed) {
        0 => Mmio::unmapped(),
        base => Mmio::at(base),
    }
}

/// Wait for `register`'s `pending` bit to clear.
fn wait_for(mmio: Mmio, register: u64, pending: u32) -> Result<(), &'static str> {
    for _ in 0..POLL_LIMIT {
        if mmio.read32(register) & pending == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err("a GICv3 register write never finished taking effect")
}

/// Map the distributor and the redistributor region at the physical addresses
/// the machine's description gave, and bring up the controller and this
/// core's side of it.
///
/// # Safety
///
/// Must be called once, on the boot CPU, after the vector table is installed
/// and while interrupts are masked: it leaves the controller able to deliver.
pub(crate) unsafe fn init(
    distributor: u64,
    redistributors: u64,
    redistributors_len: u64,
) -> Result<(), &'static str> {
    if redistributors_len < GICR_STRIDE {
        return Err("the GICv3 redistributor region is smaller than one redistributor");
    }
    let gicd = crate::vmap::map_device(distributor, GICD_WINDOW)
        .map_err(|_| "could not map the GIC distributor")?;
    let gicr = crate::vmap::map_device(redistributors, redistributors_len)
        .map_err(|_| "could not map the GIC redistributors")?;
    DISTRIBUTOR.store(gicd, Ordering::Relaxed);
    REDISTRIBUTORS.store(gicr, Ordering::Relaxed);
    REDISTRIBUTORS_LEN.store(redistributors_len, Ordering::Relaxed);

    configure_distributor(Mmio::at(gicd))?;
    init_this_cpu()
}

/// Put the distributor into a known state: every shared line off, in group 1,
/// at the default priority; then enabled with affinity routing.
fn configure_distributor(gicd: Mmio) -> Result<(), &'static str> {
    // As on a GICv2, the low five bits give the line count as 32 * (N + 1),
    // and the architecture caps it below the special identifiers.
    let lines = ((gicd.read32(GICD_TYPER) & 0b1_1111) + 1) * 32;
    let lines = lines.min(FIRST_SPECIAL_ID);

    gicd.write32(GICD_CTLR, 0);
    wait_for(gicd, GICD_CTLR, GICD_CTLR_RWP)?;

    // From 32 up only: interrupts 0..32 are the redistributors' now, and
    // these words of the distributor are reserved.
    for word in 1..lines.div_ceil(32) {
        let offset = u64::from(word) * 4;
        gicd.write32(GICD_IGROUPR + offset, u32::MAX);
        gicd.write32(GICD_ICENABLER + offset, u32::MAX);
    }
    for line in (PRIVATE_LINES..lines).step_by(4) {
        gicd.write32(
            GICD_IPRIORITYR + u64::from(line),
            u32::from_ne_bytes([DEFAULT_PRIORITY; 4]),
        );
    }
    wait_for(gicd, GICD_CTLR, GICD_CTLR_RWP)?;

    gicd.write32(GICD_CTLR, GICD_CTLR_ARE | GICD_CTLR_ENABLE_GROUP1);
    wait_for(gicd, GICD_CTLR, GICD_CTLR_RWP)
}

/// This core's redistributor, by address: the frame in the region whose
/// `GICR_TYPER` carries this core's affinity.
fn this_redistributor() -> Option<u64> {
    let base = REDISTRIBUTORS.load(Ordering::Relaxed);
    let len = REDISTRIBUTORS_LEN.load(Ordering::Relaxed);
    if base == 0 {
        return None;
    }
    // `GICR_TYPER`'s upper word packs Aff3.Aff2.Aff1.Aff0 a byte each, where
    // `MPIDR_EL1` keeps Aff3 apart from the other three.
    let mpidr = cpu::read_mpidr() & MPIDR_AFFINITY;
    let wanted = ((mpidr >> 8) & 0xFF00_0000) as u32 | (mpidr & 0x00FF_FFFF) as u32;

    let mut offset = 0;
    while offset + GICR_STRIDE <= len {
        let frame = Mmio::at(base + offset);
        let low = frame.read32(GICR_TYPER);
        if frame.read32(GICR_TYPER + 4) == wanted {
            return Some(base + offset);
        }
        if low & GICR_TYPER_LAST != 0 {
            return None;
        }
        offset += if low & GICR_TYPER_VLPIS != 0 {
            GICR_STRIDE_V4
        } else {
            GICR_STRIDE
        };
    }
    None
}

/// Bring up this core's side of the controller: wake its redistributor, put
/// its private interrupts in group 1 at the default priority with the boot
/// core's enables, and turn on its CPU interface.
///
/// On the boot core [`init`] calls this; each secondary calls it for itself.
///
/// # Errors
///
/// No redistributor for this core, one that will not wake, or a CPU interface
/// EL1 is not allowed to use.
pub(crate) fn init_this_cpu() -> Result<(), &'static str> {
    let rd_base = this_redistributor().ok_or("no GICv3 redistributor names this core")?;
    let rd = Mmio::at(rd_base);

    // A redistributor comes out of reset believing its core asleep, and a
    // core the controller thinks asleep is sent nothing.
    let waker = rd.read32(GICR_WAKER);
    rd.write32(GICR_WAKER, waker & !WAKER_PROCESSOR_SLEEP);
    let mut awake = false;
    for _ in 0..POLL_LIMIT {
        if rd.read32(GICR_WAKER) & WAKER_CHILDREN_ASLEEP == 0 {
            awake = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !awake {
        return Err("this core's GICv3 redistributor did not wake");
    }

    let sgi = Mmio::at(rd_base + GICR_SGI_FRAME);
    sgi.write32(GICD_ICENABLER, u32::MAX);
    wait_for(rd, GICR_CTLR, GICR_CTLR_RWP)?;
    sgi.write32(GICD_IGROUPR, u32::MAX);
    for line in (0..PRIVATE_LINES).step_by(4) {
        sgi.write32(
            GICD_IPRIORITYR + u64::from(line),
            u32::from_ne_bytes([DEFAULT_PRIORITY; 4]),
        );
    }
    let enabled = PRIVATE_ENABLED.load(Ordering::Relaxed);
    if enabled != 0 {
        sgi.write32(GICD_ISENABLER, enabled);
    }

    if cpu::enable_gicv3_cpu_interface() & 1 == 0 {
        return Err("EL1 is not allowed the GICv3 system register interface");
    }
    Ok(())
}

/// The window holding interrupt `id`'s enable and priority registers: this
/// core's SGI frame below 32, the distributor from there up.
fn registers_for(id: u32) -> Mmio {
    if id < PRIVATE_LINES {
        this_redistributor().map_or(Mmio::unmapped(), |rd| Mmio::at(rd + GICR_SGI_FRAME))
    } else {
        window(&DISTRIBUTOR)
    }
}

/// Let interrupt `id` through, to this core if it is a shared one.
pub(crate) fn enable(id: u32) {
    let registers = registers_for(id);
    let bit = 1_u32 << (id % 32);

    let register = GICD_IPRIORITYR + u64::from(id & !3);
    let mut priorities = registers.read32(register).to_ne_bytes();
    if let Some(slot) = priorities.get_mut((id % 4) as usize) {
        *slot = DEFAULT_PRIORITY;
    }
    registers.write32(register, u32::from_ne_bytes(priorities));

    if id >= PRIVATE_LINES {
        // The routing register resets to an UNKNOWN core, and the GICv2
        // driver's promise is that a shared interrupt goes to the core that
        // enables it. Written as two halves: the architecture allows 32-bit
        // access to every 64-bit distributor register.
        let affinity = cpu::read_mpidr() & MPIDR_AFFINITY;
        let router = GICD_IROUTER + u64::from(id) * 8;
        registers.write32(router, affinity as u32);
        registers.write32(router + 4, (affinity >> 32) as u32);
    }

    registers.write32(GICD_ISENABLER + u64::from(id / 32) * 4, bit);

    if id < PRIVATE_LINES {
        let _ = PRIVATE_ENABLED.fetch_or(bit, Ordering::Relaxed);
    }
}

/// Stop delivering `id` until [`enable`] turns it back on.
pub(crate) fn disable(id: u32) {
    let bit = 1_u32 << (id % 32);
    registers_for(id).write32(GICD_ICENABLER + u64::from(id / 32) * 4, bit);
    if id < PRIVATE_LINES {
        let _ = PRIVATE_ENABLED.fetch_and(!bit, Ordering::Relaxed);
    }
}

/// Claim the interrupt that arrived, or `None` if there was none.
///
/// The whole register value is kept for [`complete`], as the GICv2 driver
/// keeps its own.
pub(crate) fn claim() -> Option<(u32, u32)> {
    let acknowledgement = cpu::read_icc_iar1();
    let id = (acknowledgement & IAR_ID_MASK) as u32;
    if (FIRST_SPECIAL_ID..1024).contains(&id) {
        return None;
    }
    Some((id, id))
}

/// Tell the controller the interrupt has been handled.
pub(crate) fn complete(acknowledgement: u32) {
    cpu::write_icc_eoir1(u64::from(acknowledgement));
}

/// Interrupt every core but this one on [`IPI_SGI`].
///
/// The caller orders its own stores first, with the same `dsb` as for a
/// GICv2: a system register write is no more ordered against memory than a
/// device write is.
pub(crate) fn send_sgi_to_others() {
    cpu::write_icc_sgi1r(SGI1R_ALL_BUT_SELF | (u64::from(IPI_SGI) << SGI1R_INTID_SHIFT));
}
