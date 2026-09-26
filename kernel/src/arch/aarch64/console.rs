//! The PL011 UART, or on a machine without one the kernel can reach, a
//! console record in `ramoops` memory.
//!
//! One of the two device drivers inside the kernel — see `crate::console` for
//! why it is here at all rather than in userspace.
//!
//! The `ramoops` backend is for the Pixel 7, whose UART is behind a debug
//! accessory on its USB-C port. Its loader passes
//! `console=ramoops,<address>,<size>`, naming the console zone of the region
//! Android's kernel keeps its log in, and has already started a record there;
//! this appends to it, in Linux's `persistent_ram_buffer` format, so the boot
//! log survives the watchdog reset that ends a run and Android shows it as
//! `/sys/fs/pstore/console-ramoops-0`. It has no input and never waits.
//!
//! Unlike x86-64's 16550, this one is `MMIO`, so it has to be *mapped* before
//! it can be written, and mapped as device memory: through a normal cacheable
//! mapping the writes may be merged, reordered or held in a cache line, and the
//! symptom is a console that prints nothing at all.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_bootinfo::{BootView, KERNEL_VMAP_BASE, PAGE_SIZE};

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

/// Data register: writing transmits, reading takes a received byte.
const DR: u64 = 0x000;
/// `DR` as read: the framing, parity, break and overrun flags the byte arrived
/// with, above the byte itself.
const DR_ERRORS: u32 = 0xF00;
/// Receive status register; writing it, as the error clear register, clears
/// the flags.
const RSR_ECR: u64 = 0x004;
/// Flag register.
const FR: u64 = 0x018;
/// `FR`: the transmit FIFO is full.
const FR_TXFF: u32 = 1 << 5;
/// `FR`: the receive FIFO is empty.
const FR_RXFE: u32 = 1 << 4;
/// `FR`: the transmit FIFO is empty.
const FR_TXFE: u32 = 1 << 7;
/// `FR`: the port is still sending a byte, FIFO empty or not.
const FR_BUSY: u32 = 1 << 3;
/// Interrupt mask set/clear register: a set bit lets that interrupt out.
const IMSC: u64 = 0x038;
/// `IMSC`: the receive FIFO reached its trigger level.
const IMSC_RX: u32 = 1 << 4;
/// `IMSC`: bytes have sat in the receive FIFO, below the trigger level, for
/// longer than a few characters' time — the interrupt a single keystroke raises.
const IMSC_RT: u32 = 1 << 6;
/// `IMSC`: the transmit FIFO has drained to its trigger level.
const IMSC_TX: u32 = 1 << 5;

/// The mapped base address, once [`init`] has run.
struct Base(UnsafeCell<u64>);

// SAFETY: early boot is single-threaded — no other CPU has been started and
// interrupts are masked — so there is never a second accessor.
unsafe impl Sync for Base {}

static BASE: Base = Base(UnsafeCell::new(0));

/// `PERSISTENT_RAM_SIG`, "DBGC": the first word of a `ramoops` record.
const RAMOOPS_SIGNATURE: u32 = 0x4347_4244;

/// Bytes of a `ramoops` record's header: the signature, the write offset and
/// the number of valid bytes.
const RAMOOPS_HEADER: u64 = 12;

/// Bytes of text the `ramoops` zone holds, or zero when the console is the
/// PL011. Set once by [`init`], before any output.
static RAMOOPS_CAPACITY: AtomicU64 = AtomicU64::new(0);

/// Bytes of text in the `ramoops` record. Output is serialised by
/// `crate::console`, so a plain load and store are enough.
static RAMOOPS_LENGTH: AtomicU64 = AtomicU64::new(0);

/// Parse `ramoops,<address>,<size>`, the loader's `console=` value, both
/// numbers in hexadecimal with a `0x` prefix.
pub(super) fn ramoops_zone(value: &str) -> Option<(u64, u64)> {
    let mut fields = value.strip_prefix("ramoops,")?.split(',');
    let number = |text: &str| u64::from_str_radix(text.strip_prefix("0x")?, 16).ok();
    let base = number(fields.next()?)?;
    let size = number(fields.next()?)?;
    (fields.next().is_none() && base.is_multiple_of(PAGE_SIZE) && size > RAMOOPS_HEADER)
        .then_some((base, size))
}

/// Map the console the command line names -- the PL011, or a `ramoops`
/// zone -- and record where it landed.
pub(crate) fn init(view: &BootView<'_>, memory: &mut EarlyMemory) -> Result<(), EarlyError> {
    if let Some((base, size)) = view.option("console").and_then(ramoops_zone) {
        // Device memory, so every byte reaches RAM in order and survives the
        // reset with no cache to be cleaned first.
        memory.map_device(PL011_VIRT, base, size)?;
        // SAFETY: single-threaded, as documented on the `Sync` impl above.
        unsafe { *BASE.0.get() = PL011_VIRT };
        let capacity = size - RAMOOPS_HEADER;
        // Continue the loader's record rather than start another, so the two
        // programs' logs read as one.
        let length = if read(0) == RAMOOPS_SIGNATURE {
            u64::from(read(8)).min(capacity)
        } else {
            0
        };
        RAMOOPS_LENGTH.store(length, Ordering::Relaxed);
        RAMOOPS_CAPACITY.store(capacity, Ordering::Relaxed);
        write(0, RAMOOPS_SIGNATURE);
        return Ok(());
    }
    memory.map_device(PL011_VIRT, PL011_PHYS, PAGE_SIZE)?;
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    unsafe { *BASE.0.get() = PL011_VIRT };
    Ok(())
}

/// True when the console is a `ramoops` record rather than a UART.
pub(crate) fn is_ramoops() -> bool {
    RAMOOPS_CAPACITY.load(Ordering::Relaxed) != 0
}

/// Append one byte to the `ramoops` record, dropping it once the zone is full.
fn ramoops_append(byte: u8) {
    let length = RAMOOPS_LENGTH.load(Ordering::Relaxed);
    if length >= RAMOOPS_CAPACITY.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    let base = unsafe { *BASE.0.get() };
    // SAFETY: the byte is inside the zone `init` mapped, past its header and
    // below its capacity.
    unsafe { core::ptr::write_volatile((base + RAMOOPS_HEADER + length) as *mut u8, byte) };
    let length = length + 1;
    RAMOOPS_LENGTH.store(length, Ordering::Relaxed);
    // `ramoops` zones are far smaller than 4 GiB.
    let word = u32::try_from(length).unwrap_or(u32::MAX);
    write(4, word);
    write(8, word);
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
    if is_ramoops() {
        ramoops_append(byte);
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

/// Wait until everything written has left the port, for a caller about to
/// power off or stop.
///
/// [`write_byte`] waits only for room in the FIFO, so the last line of a report
/// can still be sending when it returns, and a power-off straight after cuts it
/// off: seen on the STM32 USART of a DK1, and the same shape on any real
/// PL011. Bounded, like the write.
pub(crate) fn drain() {
    /// About half a second of polling; a sixteen-byte FIFO empties in under two
    /// milliseconds at 115200 baud.
    const DRAIN_LIMIT: u32 = 10_000_000;

    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 || is_ramoops() {
        return;
    }
    for _ in 0..DRAIN_LIMIT {
        if read(FR) & (FR_TXFE | FR_BUSY) == FR_TXFE {
            return;
        }
        core::hint::spin_loop();
    }
}

/// One received byte, if the receive FIFO holds one.
///
/// A byte that arrived with an error flag is still delivered, and the flags are
/// cleared: a character mangled by line noise is better seen than silently
/// dropped, and a break reads as the NUL the port puts in the FIFO for it.
pub(crate) fn read_byte() -> Option<u8> {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 || is_ramoops() {
        return None;
    }

    if read(FR) & FR_RXFE != 0 {
        return None;
    }
    let data = read(DR);
    if data & DR_ERRORS != 0 {
        write(RSR_ECR, 0);
    }
    Some(data.to_le_bytes()[0])
}

/// Let the port interrupt when it has received something.
///
/// Both receive interrupts, because each alone loses: the FIFO-level one never
/// fires for a byte or two typed by hand, and the timeout one waits out a gap
/// in a stream that has already filled the FIFO. Reading the FIFO empty clears
/// both, so the handler needs nothing but [`read_byte`].
pub(crate) fn enable_receive_interrupt() {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 || is_ramoops() {
        return;
    }
    write(IMSC, read(IMSC) | IMSC_RX | IMSC_RT);
}

/// How many bytes the port can take now without anyone waiting: one while the
/// transmit FIFO is not full, which the caller asks again after each.
pub(crate) fn transmit_room() -> usize {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return 0;
    }
    // A record in memory always has room: a full zone drops the byte instead.
    if is_ramoops() {
        return 1;
    }
    usize::from(read(FR) & FR_TXFF == 0)
}

/// Hand the port one byte, for a caller [`transmit_room`] said it had room
/// for.
pub(crate) fn put(byte: u8) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    if is_ramoops() {
        ramoops_append(byte);
        return;
    }
    write(DR, u32::from(byte));
}

/// Let the port interrupt when its transmit FIFO has drained to the trigger
/// level, or stop it.
///
/// The interrupt is raised as the FIFO drains *through* the level, and a real
/// PL011 turned on with its FIFO already below it says nothing: Linux's driver
/// fills the FIFO before it turns the interrupt on, and so does
/// `console::output`, which leaves it on only while it has more than the FIFO
/// took. QEMU's port sends at once and raises it on every write.
pub(crate) fn transmit_interrupt(on: bool) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 || is_ramoops() {
        return;
    }
    let mask = read(IMSC);
    write(IMSC, if on { mask | IMSC_TX } else { mask & !IMSC_TX });
}

/// Whether the port still has a reason to interrupt: never worth asking, since
/// its line is level-triggered and raises the interrupt again by itself. See
/// `console::input`'s handler for the port that has to be asked.
#[expect(
    clippy::missing_const_for_fn,
    reason = "another architecture's version of this reads the port"
)]
pub(crate) fn interrupt_pending() -> bool {
    false
}

/// What the port sends from, for the boot line that says how the console
/// sends.
pub(crate) fn transmit_buffer() -> &'static str {
    if is_ramoops() {
        "a ramoops record"
    } else {
        "a PL011's FIFO"
    }
}
