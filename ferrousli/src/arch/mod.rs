//! What differs between processor architectures in running threads: the
//! thread pointer, the entry trampoline `clone` returns into, the exit that
//! unmaps its own stack, and where a signal context keeps the registers.
//!
//! Everything else in the thread code is written once. Each architecture's
//! module offers the same functions and constants.

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;
