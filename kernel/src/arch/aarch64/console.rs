//! The PL011 UART.
//!
//! One of the two device drivers inside the kernel — see `crate::console` for
//! why it is here at all rather than in userspace.
//!
//! Unlike x86-64's 16550, this one is `MMIO`, so it has to be *mapped* before
//! it can be written, and mapped as device memory: through a normal cacheable
//! mapping the writes may be merged, reordered or held in a cache line, and the
//! symptom is a console that prints nothing at all.

use core::cell::UnsafeCell;

use ferrix_bootinfo::{KERNEL_VMAP_BASE, PAGE_SIZE};

use crate::early::{EarlyError, EarlyMemory};

/// Physical address of the PL011 on `QEMU`'s `virt` machine.
///
/// **A stage 1 shortcut, and the only hardcoded device address in the tree.**
/// The right answer comes from the device tree's `stdout-path` or from ACPI's
/// SPCR table, both of which the loader already hands over
/// (`BootInfo::dtb`, `BootInfo::rsdp`); parsing them is stage 3 work, and until
/// then the boot test needs somewhere to talk.
const PL011_PHYS: u64 = 0x0900_0000;

/// Where the register window is mapped. The bottom of the kernel's dynamic
/// mapping area, which nothing else uses yet.
const PL011_VIRT: u64 = KERNEL_VMAP_BASE;

/// Data register: writing transmits.
const DR: u64 = 0x000;
/// Flag register.
const FR: u64 = 0x018;
/// `FR`: the transmit FIFO is full.
const FR_TXFF: u32 = 1 << 5;

/// The mapped base address, once [`init`] has run.
struct Base(UnsafeCell<u64>);

// SAFETY: early boot is single-threaded — no other CPU has been started and
// interrupts are masked — so there is never a second accessor.
unsafe impl Sync for Base {}

static BASE: Base = Base(UnsafeCell::new(0));

/// Map the register window and record where it landed.
pub(crate) fn init(memory: &mut EarlyMemory) -> Result<(), EarlyError> {
    memory.map_device(PL011_VIRT, PL011_PHYS, PAGE_SIZE)?;
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    unsafe { *BASE.0.get() = PL011_VIRT };
    Ok(())
}

/// Read one of the UART's registers.
fn read(offset: u64) -> u32 {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    let base = unsafe { *BASE.0.get() };
    // SAFETY: `base` is either zero, handled by the caller, or the device
    // window `init` mapped, and `offset` is a register inside its first page.
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

/// Write one of the UART's registers.
fn write(offset: u64, value: u32) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    let base = unsafe { *BASE.0.get() };
    // SAFETY: as in `read`.
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, value) };
}

/// Send one byte, waiting for room in the transmit FIFO.
///
/// Firmware configured the baud rate and line format before handing over and
/// this driver does not disturb them: reprogramming a port a terminal is
/// already attached to is how a boot log turns into line noise halfway through.
///
/// The wait is bounded rather than a bare loop, because a panic that hangs
/// inside the console because nothing answered is worse than one nobody reads.
pub(crate) fn write_byte(byte: u8) {
    const SPIN_LIMIT: u32 = 100_000;

    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }

    for _ in 0..SPIN_LIMIT {
        if read(FR) & FR_TXFF == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    write(DR, u32::from(byte));
}
