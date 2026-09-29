//! Device register accesses: volatile, which KVM here emulates whatever
//! instruction LLVM chooses, since it decodes the instruction itself (the
//! Arm pair's are assembly, and `crate::mmio` says why).

/// A volatile byte read of `at`.
///
/// # Safety
///
/// (DEVICE) `at` is a mapped device address, aligned to the access's width.
pub(crate) unsafe fn read8(at: u64) -> u8 {
    // SAFETY: (DEVICE) the caller's.
    unsafe { core::ptr::read_volatile(at as *const u8) }
}

/// A volatile byte write of `value` to `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn write8(at: u64, value: u8) {
    // SAFETY: (DEVICE) the caller's.
    unsafe { core::ptr::write_volatile(at as *mut u8, value) };
}

/// A volatile halfword read of `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn read16(at: u64) -> u16 {
    // SAFETY: (DEVICE) the caller's.
    unsafe { core::ptr::read_volatile(at as *const u16) }
}

/// A volatile halfword write of `value` to `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn write16(at: u64, value: u16) {
    // SAFETY: (DEVICE) the caller's.
    unsafe { core::ptr::write_volatile(at as *mut u16, value) };
}

/// A volatile word read of `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn read32(at: u64) -> u32 {
    // SAFETY: (DEVICE) the caller's.
    unsafe { core::ptr::read_volatile(at as *const u32) }
}

/// A volatile word write of `value` to `at`.
///
/// # Safety
///
/// (DEVICE) as [`read8`].
pub(crate) unsafe fn write32(at: u64, value: u32) {
    // SAFETY: (DEVICE) the caller's.
    unsafe { core::ptr::write_volatile(at as *mut u32, value) };
}
