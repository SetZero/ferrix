//! The free-running counter on x86-64.
//!
//! Two candidates, in order of preference.
//!
//! The **HPET** is a counter in the chipset with a period firmware states
//! exactly, in femtoseconds, in a table. Nothing has to be calibrated: the
//! frequency is read, not measured, which means it carries no error at all.
//!
//! The **TSC** is a counter in the CPU. It is faster to read and it is what a
//! grown kernel ends up using, but nothing tells you its frequency — it has to
//! be measured against something else, and the only clock guaranteed to exist
//! for that is the PIT, a part designed in 1981. So the TSC is the fallback,
//! for a machine with no HPET table.
//!
//! Either way the answer is a counter and a frequency, and the rest of the
//! kernel is told neither which one it got nor that there was a choice.
//!
//! # A 32-bit HPET
//!
//! The HPET's main counter may be 32 bits wide — capability bit 13 says, and
//! AMD chipsets commonly are — and at 14.3 `MHz` that wraps every five minutes.
//! Handed out as the time base, it would send `CLOCK_MONOTONIC` backwards at
//! each wrap. Extending it in software needs a read at least once per wrap
//! period, and a processor running one task with its timer stopped makes none.
//! So a narrow HPET is not the counter: it is the reference the TSC is
//! calibrated against, over ten milliseconds and subtracted in 32 bits, which
//! keeps its exactly-stated period and costs the PIT nothing. Every counter
//! this module hands out is therefore 64 bits wide.

use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_acpi::hpet::{self, Capabilities};
use ferrix_acpi::{Acpi, GenericAddress};

use super::cpu;
use crate::acpi::DirectMap;
use crate::mmio::Mmio;

/// General configuration.
const HPET_CONFIG: u64 = 0x010;
/// The main counter.
const HPET_MAIN_COUNTER: u64 = 0x0F0;
/// General configuration: start counting.
const HPET_CONFIG_ENABLE: u64 = 1 << 0;
/// Bytes of register window a timer block occupies.
const HPET_WINDOW: u64 = 0x400;

/// The 8254's input frequency: 105/88 `MHz`, fixed since the PC/AT and the one
/// number on a PC that has never changed.
const PIT_HZ: u64 = 1_193_182;
/// Channel 2's counter port — the one channel whose gate software controls.
const PIT_CHANNEL2: u16 = 0x42;
/// The mode/command register.
const PIT_COMMAND: u16 = 0x43;
/// Port 0x61: channel 2's gate in bit 0, its output in bit 5.
const PIT_GATE: u16 = 0x61;
/// Channel 2, lobyte then hibyte, mode 0, binary.
const PIT_ONESHOT: u8 = 0b1011_0000;
/// Port 0x61, bit 5: channel 2's output, which goes high at terminal count.
const PIT_OUTPUT: u8 = 1 << 5;
/// Port 0x61, bit 0: channel 2's gate. Bit 1 is the speaker, left off.
const PIT_GATE_ON: u8 = 1 << 0;
/// Milliseconds to measure the TSC over.
const CALIBRATION_MILLIS: u64 = 10;

/// Virtual address of the HPET's registers, or zero when the TSC is in use.
static HPET: AtomicU64 = AtomicU64::new(0);

/// How fast the chosen counter counts.
static COUNTER_HZ: AtomicU64 = AtomicU64::new(0);

/// Bring up a counter and return its name, for the boot log.
///
/// # Errors
///
/// A string naming what could not be brought up. There is no third fallback:
/// a machine with neither an HPET nor a working PIT cannot tell the time, and
/// everything from the scheduler up would be built on a guess.
pub(crate) fn init(acpi: &Acpi<'_, DirectMap>) -> Result<&'static str, &'static str> {
    if let Some(phys) = hpet_address(acpi) {
        return start_hpet(phys);
    }
    start_tsc()
}

/// Where the HPET's registers are, if firmware describes a memory-mapped one.
fn hpet_address(acpi: &Acpi<'_, DirectMap>) -> Option<u64> {
    let hpet = acpi.hpet().ok()?;
    let address = hpet.base_address()?;
    // An I/O-space timer block is legal in the specification and has never
    // been built. Mapping a window over that physical address would be a
    // window over whatever else is there.
    if address.address_space_id != GenericAddress::SYSTEM_MEMORY || !address.is_present() {
        return None;
    }
    Some(address.address)
}

/// Map the timer block, start it, and prove it counts.
fn start_hpet(phys: u64) -> Result<&'static str, &'static str> {
    let base = crate::vmap::map_device(phys, HPET_WINDOW).map_err(|_| "could not map the HPET")?;
    let regs = Mmio::at(base);

    // The register, not the table: firmware's copy of these bits is a hint.
    let caps = Capabilities(read64(regs, hpet::CAPABILITIES));
    let hpet_hz = caps
        .counter_hz()
        .ok_or("the HPET reports a period outside the specification")?;

    let enabled = read64(regs, HPET_CONFIG) | HPET_CONFIG_ENABLE;
    write64(regs, HPET_CONFIG, enabled);

    // Firmware may leave the block present but powered down, in which case
    // every read returns the same value and every later measurement divides
    // by zero elapsed time. Better to find out here.
    let first = main_counter(regs);
    let mut spins = 0u32;
    while main_counter(regs) == first {
        spins = spins.saturating_add(1);
        if spins > 1_000_000 {
            return Err("the HPET is mapped but its counter does not advance");
        }
    }

    if caps.counter_is_64_bit() {
        HPET.store(base, Ordering::Relaxed);
        COUNTER_HZ.store(hpet_hz, Ordering::Relaxed);
        return Ok("HPET");
    }

    // Narrow: measure the TSC against it and hand out the TSC. `HPET` stays
    // zero, so `counter_now` reads the TSC. The window stays mapped all the
    // same: a mapped device window is how the kernel knows which registers
    // are its own and no driver's.
    let tsc_hz = calibrate_tsc_against_hpet(regs, hpet_hz, caps.counter_mask())
        .ok_or("the HPET is 32 bits wide, and the TSC could not be measured against it")?;
    COUNTER_HZ.store(tsc_hz, Ordering::Relaxed);
    Ok("TSC, calibrated against a 32-bit HPET")
}

/// Count TSC ticks over [`CALIBRATION_MILLIS`] of an HPET counting at
/// `hpet_hz`, whose counter is `mask` wide.
///
/// Ten milliseconds is about 143 thousand ticks of a 14.3 `MHz` counter, far
/// inside one wrap, so a single wrap-aware subtraction is exact however close
/// to the top the counter started.
fn calibrate_tsc_against_hpet(regs: Mmio, hpet_hz: u64, mask: u64) -> Option<u64> {
    let window = hpet_hz * CALIBRATION_MILLIS / 1000;
    let hpet_start = main_counter(regs);
    let tsc_start = cpu::rdtsc();

    let mut spins = 0u32;
    let hpet_elapsed = loop {
        let elapsed = hpet::ticks_between(hpet_start, main_counter(regs), mask);
        if elapsed >= window {
            break elapsed;
        }
        spins = spins.saturating_add(1);
        // As `calibrate_tsc`: a counter that stops is not one to hang on.
        if spins > 200_000_000 {
            return None;
        }
    };
    let tsc_elapsed = cpu::rdtsc().checked_sub(tsc_start)?;

    hpet::measured_hz(tsc_elapsed, hpet_elapsed, hpet_hz)
}

/// Measure the TSC against the PIT and use that.
fn start_tsc() -> Result<&'static str, &'static str> {
    let hz = calibrate_tsc().ok_or("no HPET, and the PIT did not answer")?;
    COUNTER_HZ.store(hz, Ordering::Relaxed);
    Ok("TSC")
}

/// Whether channel 2's output is high, meaning it reached terminal count.
fn pit_finished() -> bool {
    // SAFETY: port 0x61 is the PC/AT system control port. Reading it has no
    // side effect; bit 5 is channel 2's output.
    let status = unsafe { cpu::inb(PIT_GATE) };
    status & PIT_OUTPUT != 0
}

/// Count TSC ticks over a known PIT interval.
///
/// Channel 2 rather than channel 0: it is the only one whose gate software
/// controls and the only one not wired to an interrupt, so measuring with it
/// cannot disturb anything.
fn calibrate_tsc() -> Option<u64> {
    let count = u16::try_from(PIT_HZ * CALIBRATION_MILLIS / 1000).ok()?;

    // SAFETY: reading port 0x61 to preserve the bits this does not own —
    // the speaker's, principally, which must come back as it was found.
    let saved = unsafe { cpu::inb(PIT_GATE) };
    // SAFETY: gate on, speaker off. Bits 2..7 are preserved.
    unsafe { cpu::outb(PIT_GATE, (saved & 0xFC) | PIT_GATE_ON) };
    // SAFETY: the 8254 command register, programming channel 2 one-shot.
    unsafe { cpu::outb(PIT_COMMAND, PIT_ONESHOT) };
    // SAFETY: channel 2's counter, low byte then high byte as the command
    // just asked for. The second write starts it.
    unsafe { cpu::outb(PIT_CHANNEL2, count as u8) };
    // SAFETY: as above, the high byte.
    unsafe { cpu::outb(PIT_CHANNEL2, (count >> 8) as u8) };

    let start = cpu::rdtsc();
    let mut spins = 0u32;
    while !pit_finished() {
        spins = spins.saturating_add(1);
        // About a second of spinning on any real CPU. A PIT that never
        // finishes is a machine that cannot be calibrated, not one to hang on.
        if spins > 200_000_000 {
            return None;
        }
    }
    let elapsed = cpu::rdtsc().checked_sub(start)?;

    // SAFETY: put port 0x61 back exactly as it was found.
    unsafe { cpu::outb(PIT_GATE, saved) };

    if elapsed == 0 {
        return None;
    }
    Some(elapsed * 1000 / CALIBRATION_MILLIS)
}

/// Read one of the HPET's 64-bit registers.
///
/// The only 64-bit device registers in the tree, which is why these are here
/// rather than on [`Mmio`]: on AArch64 they would be dead code, and a shared
/// module carrying an accessor nobody calls is how a facade starts to rot.
fn read64(regs: Mmio, offset: u64) -> u64 {
    (u64::from(regs.read32(offset + 4)) << 32) | u64::from(regs.read32(offset))
}

/// Write one of them.
fn write64(regs: Mmio, offset: u64, value: u64) {
    regs.write32(offset, value as u32);
    regs.write32(offset + 4, (value >> 32) as u32);
}

/// The main counter, read so that a 32-bit wrap cannot be observed torn.
///
/// The two halves are read separately, so the low half can wrap between them
/// and produce a value nearly four billion ticks wrong. Re-reading the high
/// half and retrying when it changed is the standard answer, and it
/// terminates: the high half advances once every few minutes.
fn main_counter(regs: Mmio) -> u64 {
    loop {
        let high = regs.read32(HPET_MAIN_COUNTER + 4);
        let low = regs.read32(HPET_MAIN_COUNTER);
        if regs.read32(HPET_MAIN_COUNTER + 4) == high {
            return (u64::from(high) << 32) | u64::from(low);
        }
    }
}

/// The counter's current value.
pub(crate) fn counter_now() -> u64 {
    let base = HPET.load(Ordering::Relaxed);
    if base == 0 {
        return cpu::rdtsc();
    }
    main_counter(Mmio::at(base))
}

/// How fast it counts, or zero before [`init`].
pub(crate) fn counter_hz() -> u64 {
    COUNTER_HZ.load(Ordering::Relaxed)
}

/// The width of the counter [`counter_now`] reads: all 64 bits, whichever it
/// is, because a 32-bit HPET is only ever a calibration reference.
const COUNTER_MASK: u64 = u64::MAX;

/// Ticks from one [`counter_now`] reading to a later one, subtracted in the
/// counter's own width.
///
/// The one place a caller measuring an interval should subtract, so the width
/// is decided here, beside the choice of counter, and not assumed at each
/// call site.
pub(crate) fn ticks_between(earlier: u64, later: u64) -> u64 {
    hpet::ticks_between(earlier, later, COUNTER_MASK)
}
