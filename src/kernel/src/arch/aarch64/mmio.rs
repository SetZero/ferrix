//! Device register accesses, each one `ldr` or `str` with a bare base
//! register: the form KVM can emulate from the fault's syndrome
//! (`crate::mmio` says why volatile is not enough here).
//!
//! Every function is unsafe for the one reason its `# Safety` gives, and
//! none is `nomem` or `readonly`, so the compiler keeps each access in
//! program order with every other access to memory, as volatile did.

use core::arch::asm;

/// `ldrb` from `at`.
///
/// # Safety
///
/// (DEVICE) `at` is a mapped device address, aligned to the access's width.
pub(crate) unsafe fn read8(at: u64) -> u8 {
    let value: u32;
    // SAFETY: (DEVICE) the caller's; one load, base register only, no stack.
    unsafe {
        asm!("ldrb {v:w}, [{a}]", a = in(reg) at, v = out(reg) value, options(nostack, preserves_flags));
    }
    value as u8
}

/// `strb` of `value` to `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn write8(at: u64, value: u8) {
    // SAFETY: (DEVICE) as `read8`.
    unsafe {
        asm!("strb {v:w}, [{a}]", a = in(reg) at, v = in(reg) u32::from(value), options(nostack, preserves_flags));
    }
}

/// `ldrh` from `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn read16(at: u64) -> u16 {
    let value: u32;
    // SAFETY: (DEVICE) as `read8`.
    unsafe {
        asm!("ldrh {v:w}, [{a}]", a = in(reg) at, v = out(reg) value, options(nostack, preserves_flags));
    }
    value as u16
}

/// `strh` of `value` to `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn write16(at: u64, value: u16) {
    // SAFETY: (DEVICE) as `read8`.
    unsafe {
        asm!("strh {v:w}, [{a}]", a = in(reg) at, v = in(reg) u32::from(value), options(nostack, preserves_flags));
    }
}

/// `ldr` of a word from `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn read32(at: u64) -> u32 {
    let value: u32;
    // SAFETY: (DEVICE) as `read8`.
    unsafe {
        asm!("ldr {v:w}, [{a}]", a = in(reg) at, v = out(reg) value, options(nostack, preserves_flags));
    }
    value
}

/// `str` of a word to `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn write32(at: u64, value: u32) {
    // SAFETY: (DEVICE) as `read8`.
    unsafe {
        asm!("str {v:w}, [{a}]", a = in(reg) at, v = in(reg) value, options(nostack, preserves_flags));
    }
}
