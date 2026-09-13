//! The HPET's own capability register, and counting with a counter that may
//! be 32 bits wide.
//!
//! The description table says where the timer block is and what it claims;
//! the block's general capabilities register at offset zero is the answer.
//! Reading that register is `MMIO` and the kernel's business. What it *means*
//! is a pure function of 64 bits, and so is the arithmetic a narrow counter
//! needs, which is exactly the part that goes wrong silently: a 32-bit main
//! counter wraps every five minutes at a typical 14.3 `MHz`, and two reads
//! subtracted in 64 bits across the wrap give an interval of about 584
//! millennia.
//!
//! ```
//! # use ferrix_acpi::hpet::{Capabilities, ticks_between};
//! let caps = Capabilities(0x0429_B17F_8086_0701); // an AMD part: 32-bit counter
//! assert!(!caps.counter_is_64_bit());
//! assert_eq!(ticks_between(0xFFFF_FF00, 0x0000_0100, caps.counter_mask()), 0x200);
//! ```

/// Offset of the general capabilities and ID register.
pub const CAPABILITIES: u64 = 0x000;

/// Capabilities: the main counter is 64 bits wide rather than 32
/// (`COUNT_SIZE_CAP`).
const COUNTER_64BIT: u64 = 1 << 13;

/// Capabilities: the main counter's period, in femtoseconds, is the top half.
const PERIOD_SHIFT: u32 = 32;

/// The slowest period the specification permits: 100 ns, or 10 `MHz`.
///
/// Firmware reporting something slower has not described an HPET, and dividing
/// by whatever it did say would produce a frequency the rest of the kernel
/// would then trust.
pub const MAX_PERIOD_FS: u64 = 0x05F5_E100;

/// Femtoseconds in a second, the unit the period is stated in.
const FEMTOS_PER_SECOND: u64 = 1_000_000_000_000_000;

/// The general capabilities and ID register, as read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities(pub u64);

impl Capabilities {
    /// Whether the main counter is 64 bits wide.
    #[must_use]
    pub const fn counter_is_64_bit(self) -> bool {
        self.0 & COUNTER_64BIT != 0
    }

    /// The bits of the main counter that count: all 64, or the low 32.
    ///
    /// A 32-bit counter's upper register reads as zero, so a 64-bit read of it
    /// is harmless; what is not harmless is believing the difference of two.
    #[must_use]
    pub const fn counter_mask(self) -> u64 {
        if self.counter_is_64_bit() {
            u64::MAX
        } else {
            u32::MAX as u64
        }
    }

    /// The main counter's period in femtoseconds, if the specification allows
    /// it: non-zero and no slower than [`MAX_PERIOD_FS`].
    #[must_use]
    pub const fn period_fs(self) -> Option<u64> {
        let period = self.0 >> PERIOD_SHIFT;
        if period == 0 || period > MAX_PERIOD_FS {
            None
        } else {
            Some(period)
        }
    }

    /// How fast the main counter counts, if the period is one an HPET can
    /// have.
    #[must_use]
    pub const fn counter_hz(self) -> Option<u64> {
        match self.period_fs() {
            Some(period) => Some(FEMTOS_PER_SECOND / period),
            None => None,
        }
    }
}

/// Ticks a counter `mask` wide advanced from `earlier` to `later`.
///
/// Correct across one wrap, which is all two reads can tell apart: a counter
/// that wrapped twice between them looks the same as one that wrapped once.
/// Keeping reads closer together than a wrap period is the caller's half.
#[must_use]
pub const fn ticks_between(earlier: u64, later: u64, mask: u64) -> u64 {
    later.wrapping_sub(earlier) & mask
}

/// The frequency of a counter that advanced `counted` while a reference
/// counting at `reference_hz` advanced `reference_ticks`.
///
/// `None` for a reference that did not move, or an answer that does not fit.
/// Done in 128 bits: a TSC counts in gigahertz, and the product overflows 64
/// bits over a window of a few seconds.
#[must_use]
pub fn measured_hz(counted: u64, reference_ticks: u64, reference_hz: u64) -> Option<u64> {
    if reference_ticks == 0 {
        return None;
    }
    let hz = u128::from(counted) * u128::from(reference_hz) / u128::from(reference_ticks);
    u64::try_from(hz).ok().filter(|&hz| hz != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QEMU's block: 64-bit counter, a 10 ns period, three comparators.
    const QEMU: Capabilities = Capabilities(0x0098_9680_8086_A201);

    /// The same block with the counter-size bit clear, as an AMD chipset's.
    const NARROW: Capabilities = Capabilities(0x0429_B17F_8086_0701);

    #[test]
    fn the_counter_width_is_read_from_bit_thirteen() {
        assert!(QEMU.counter_is_64_bit());
        assert_eq!(QEMU.counter_mask(), u64::MAX);
        assert!(!NARROW.counter_is_64_bit());
        assert_eq!(NARROW.counter_mask(), 0xFFFF_FFFF);
    }

    #[test]
    fn a_narrow_counter_is_subtracted_in_its_own_width() {
        // Across the wrap: 0x100 ticks to the top and 0x100 past it.
        assert_eq!(
            ticks_between(0xFFFF_FF00, 0x100, NARROW.counter_mask()),
            0x200
        );
        // And a 64-bit subtraction of the same reads is the bug: nearly 2^64.
        assert!(0x100u64.wrapping_sub(0xFFFF_FF00) > u64::from(u32::MAX));
        // Without a wrap both widths agree.
        assert_eq!(ticks_between(0x1000, 0x3000, NARROW.counter_mask()), 0x2000);
        assert_eq!(ticks_between(0x1000, 0x3000, QEMU.counter_mask()), 0x2000);
    }

    #[test]
    fn a_wide_counter_is_subtracted_in_all_64_bits() {
        let earlier = 0x0000_0001_FFFF_FF00;
        let later = 0x0000_0002_0000_0100;
        assert_eq!(ticks_between(earlier, later, QEMU.counter_mask()), 0x200);
        assert_eq!(ticks_between(u64::MAX - 1, 1, QEMU.counter_mask()), 3);
    }

    #[test]
    fn the_period_is_the_top_half_and_is_bounded() {
        assert_eq!(QEMU.period_fs(), Some(10_000_000));
        assert_eq!(QEMU.counter_hz(), Some(100_000_000));
        // 0x0429_B17F fs is 69.84 ns: the 14.318 MHz crystal a PC has, less
        // the fraction integer division drops.
        assert_eq!(NARROW.counter_hz(), Some(14_318_179));
        assert_eq!(Capabilities(0x8086_A201).period_fs(), None, "zero");
        assert_eq!(
            Capabilities((MAX_PERIOD_FS + 1) << 32).period_fs(),
            None,
            "slower than the specification allows"
        );
    }

    #[test]
    fn a_frequency_is_measured_against_a_reference_without_overflow() {
        // Ten milliseconds of a 100 MHz HPET, against a 3.2 GHz TSC.
        let hpet_hz = 100_000_000;
        let window = hpet_hz / 100;
        let tsc = 32_000_000;
        assert_eq!(measured_hz(tsc, window, hpet_hz), Some(3_200_000_000));
        // A product far past 64 bits still divides back.
        assert_eq!(
            measured_hz(u64::MAX / 2, 1 << 40, 1 << 40),
            Some(u64::MAX / 2)
        );
        assert_eq!(
            measured_hz(1, 0, hpet_hz),
            None,
            "the reference never moved"
        );
        assert_eq!(measured_hz(u64::MAX, 1, u64::MAX), None, "does not fit");
    }
}
