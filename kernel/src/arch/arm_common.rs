//! Arm peripherals, not Arm architectures: register-level drivers for hardware
//! both Arm ISAs can have, which the architecture that uses them has already
//! found in the machine's description -- the MADT on AArch64, the device tree
//! on ARMv7-A.
//!
//! The GICv2 is shared by both. The two serial ports are ARMv7-A's, which has
//! to pick between them because the machines it targets do not agree on one:
//! `armv7a::console` is where the device tree decides, and the layering check
//! keeps generic code from naming any of the three. A GICv3 is AArch64's
//! alone, because its CPU interface is system registers, and lives in
//! `aarch64::gic`.

pub(super) mod gicv2;
#[cfg(target_arch = "arm")]
pub(super) mod pl011;
#[cfg(target_arch = "arm")]
pub(super) mod stm32_usart;
