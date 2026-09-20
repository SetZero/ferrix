//! What differs between the three architectures: the relocation numbers, and
//! whether `REL` or `RELA` is the ordinary form.
//!
//! Every number here is from `/usr/include/elf.h`, read rather than
//! remembered, and the name each is given is the name that header gives it.
//! The loader's own code names only the aliases below, so adding a fourth
//! architecture is this file and nothing else.
//!
//! The eight kinds every architecture has, under one set of names:
//!
//! * `RELATIVE` — add the load bias to what is there. No symbol.
//! * `GLOB_DAT` and `JUMP_SLOT` — a symbol's address, for data and for a
//!   call. This loader resolves both at load, so they differ only in which
//!   table they arrive in.
//! * `ABSOLUTE` — a symbol's address plus an addend, written whole.
//! * `IRELATIVE` — call the function at bias plus addend, and write what it
//!   returns. How a program picks an implementation by what the processor
//!   has.
//! * `DTPMOD`, `DTPOFF` and `TPOFF` — thread-local storage: which module, the
//!   offset inside its block, and the offset from the thread pointer.

#![allow(
    dead_code,
    reason = "every architecture's table is written whole, so the numbers an \
              architecture happens not to need are still stated"
)]

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

/// x86-64's numbers.
#[cfg(target_arch = "x86_64")]
mod x86_64 {
    /// Nothing to do.
    pub const R_NONE: u32 = 0;
    /// `R_X86_64_64`.
    pub const R_ABSOLUTE: u32 = 1;
    /// `R_X86_64_GLOB_DAT`.
    pub const R_GLOB_DAT: u32 = 6;
    /// `R_X86_64_JUMP_SLOT`.
    pub const R_JUMP_SLOT: u32 = 7;
    /// `R_X86_64_RELATIVE`.
    pub const R_RELATIVE: u32 = 8;
    /// `R_X86_64_DTPMOD64`.
    pub const R_DTPMOD: u32 = 16;
    /// `R_X86_64_DTPOFF64`.
    pub const R_DTPOFF: u32 = 17;
    /// `R_X86_64_TPOFF64`.
    pub const R_TPOFF: u32 = 18;
    /// `R_X86_64_IRELATIVE`.
    pub const R_IRELATIVE: u32 = 37;
}

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;

/// AArch64's numbers.
#[cfg(target_arch = "aarch64")]
mod aarch64 {
    /// Nothing to do.
    pub const R_NONE: u32 = 0;
    /// `R_AARCH64_ABS64`.
    pub const R_ABSOLUTE: u32 = 257;
    /// `R_AARCH64_GLOB_DAT`.
    pub const R_GLOB_DAT: u32 = 1025;
    /// `R_AARCH64_JUMP_SLOT`.
    pub const R_JUMP_SLOT: u32 = 1026;
    /// `R_AARCH64_RELATIVE`.
    pub const R_RELATIVE: u32 = 1027;
    /// `R_AARCH64_TLS_DTPMOD`.
    pub const R_DTPMOD: u32 = 1028;
    /// `R_AARCH64_TLS_DTPREL`.
    pub const R_DTPOFF: u32 = 1029;
    /// `R_AARCH64_TLS_TPREL`.
    pub const R_TPOFF: u32 = 1030;
    /// `R_AARCH64_IRELATIVE`.
    pub const R_IRELATIVE: u32 = 1032;
}

#[cfg(target_arch = "arm")]
pub use arm::*;

/// ARM's numbers.
#[cfg(target_arch = "arm")]
mod arm {
    /// Nothing to do.
    pub const R_NONE: u32 = 0;
    /// `R_ARM_ABS32`.
    pub const R_ABSOLUTE: u32 = 2;
    /// `R_ARM_TLS_DTPMOD32`.
    pub const R_DTPMOD: u32 = 17;
    /// `R_ARM_TLS_DTPOFF32`.
    pub const R_DTPOFF: u32 = 18;
    /// `R_ARM_TLS_TPOFF32`.
    pub const R_TPOFF: u32 = 19;
    /// `R_ARM_GLOB_DAT`.
    pub const R_GLOB_DAT: u32 = 21;
    /// `R_ARM_JUMP_SLOT`.
    pub const R_JUMP_SLOT: u32 = 22;
    /// `R_ARM_RELATIVE`.
    pub const R_RELATIVE: u32 = 23;
    /// `R_ARM_IRELATIVE`.
    pub const R_IRELATIVE: u32 = 160;
}
