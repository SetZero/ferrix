//! The architecture-specific end of the loader.
//!
//! Everything above this module is the same on both machines. What differs is
//! the last few hundred instructions: installing a translation regime and
//! jumping to an address that did not exist a moment earlier. That cannot be a
//! Rust function call, because the return address would be in the old address
//! space — which is why this is where the loader's only assembly lives.
//!
//! `#[cfg(target_arch)]` is confined to this directory by
//! `scripts/check-crate-layering.sh`, and the facade below is what the rest of
//! the loader sees.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{ARCH, ELF_MACHINE, clean_dcache, enter_kernel, prepare_cpu};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{ARCH, ELF_MACHINE, clean_dcache, enter_kernel, prepare_cpu};

/// The page table descriptor layout this machine uses.
#[cfg(target_arch = "x86_64")]
pub(crate) type PageEncoding = ferrix_paging::x86_64::X86_64;
/// The page table descriptor layout this machine uses.
#[cfg(target_arch = "aarch64")]
pub(crate) type PageEncoding = ferrix_paging::aarch64::AArch64;

/// Whether the identity map needs a root table of its own.
///
/// `AArch64` splits the address space between two base registers, so the
/// identity map lives in a separate `TTBR0_EL1` tree that the kernel can drop
/// wholesale once it is running. x86-64 has one tree covering both halves, and
/// the kernel unmaps the low entries instead.
#[cfg(target_arch = "x86_64")]
pub(crate) const SEPARATE_IDENTITY_TABLE: bool = false;
/// Whether the identity map needs a root table of its own.
#[cfg(target_arch = "aarch64")]
pub(crate) const SEPARATE_IDENTITY_TABLE: bool = true;

/// Everything the kernel needs to be started with.
///
/// Assembled by the loader while firmware is still alive, and consumed by
/// [`enter_kernel`] once it is not.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Handoff {
    /// Physical address of the root table for the kernel half of the address
    /// space: the x86-64 PML4, or the `AArch64` `TTBR1_EL1` table.
    pub(crate) root_table: u64,
    /// Physical address of the identity-mapping `TTBR0_EL1` table.
    /// Unused on x86-64, where one table covers both halves.
    pub(crate) identity_table: u64,
    /// Virtual address of the kernel entry point.
    pub(crate) entry: u64,
    /// Virtual address of the top of the kernel's initial stack.
    pub(crate) stack_top: u64,
    /// Virtual address of the boot info structure.
    pub(crate) boot_info: u64,
}
