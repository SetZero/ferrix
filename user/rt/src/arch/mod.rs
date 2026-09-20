//! The instructions a native program cannot say in Rust, per architecture.
//!
//! Three things, each allow-listed in `scripts/asm-allowlist.json`:
//!
//! * `_start`, because a process is *entered*, not called: the kernel drops to
//!   user mode at the entry point with a stack pointer and registers it chose,
//!   no return address, and no frame. A Rust function's prologue assumes all
//!   three are a caller's.
//! * The trap instruction and its register assignment, which is the kernel's
//!   system call entry read from the other side.
//! * `exit_group`, the same trap with nothing to return to.
//!
//! The trap is one block per architecture and serves both ABIs: `call` makes
//! a native call and `linux` a Linux one, and the kernel tells them apart by
//! the number's range and by nothing else (`docs/CLIPBOARD.md` §5).
//!
//! This is the facade: the one place under `user/` that selects on
//! `target_arch`, for the reason `kernel/src/arch/mod.rs` is the kernel's.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "arm")]
mod armv7a;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{call, exit, linux};
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::{call, exit, linux};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{call, exit, linux};
