//! AArch64.
//!
//! The kernel's entry is `system_call` in `kernel/src/arch/aarch64/trap.rs`:
//! `svc #0`, the number in X8, the arguments in X0 to X5, and the result back
//! in X0. Every other register comes back as it went.

use core::arch::{asm, naked_asm};

use ferrix_linux_abi::nr::aarch64::EXIT_GROUP;
use ferrix_native::Raw;

/// The process's first instruction.
///
/// `ferrix_enter_user` starts a program with `eret`, which leaves the frame
/// pointer and link register as whatever it put in them rather than a caller's
/// frame. They are zeroed so a frame-pointer walk ends here, the stack pointer
/// is brought to a multiple of 16 in case a starter did not, and
/// `ferrix_rt_start` is called. The bootstrap handle's register, X0, is already
/// the first argument.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub(crate) extern "C" fn _start() -> ! {
    naked_asm!(
        "mov x29, #0",
        "mov x30, #0",
        "mov x9, sp",
        "bic x9, x9, #15",
        "mov sp, x9",
        "bl ferrix_rt_start",
        "udf #0",
    )
}

/// Make the call `raw` describes.
pub(crate) fn call(raw: &Raw<'_>) -> usize {
    let [a0, a1, a2, a3, a4, a5] = raw.args();
    let result;
    // SAFETY: `raw` was built by `libs/native`, which puts in a pointer
    // argument only the address of a slice `raw` borrows — shared if the
    // kernel reads it, exclusive if it writes — and `raw` outlives this trap.
    // Every native call either touches only that memory or adds mappings where
    // nothing is mapped, so no memory Rust believes it owns changes under it.
    // The kernel preserves every register but X0, and uses no user stack.
    unsafe {
        asm!(
            "svc #0",
            in("x8") raw.number(),
            inlateout("x0") a0 => result,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
            in("x4") a4,
            in("x5") a5,
            options(nostack),
        );
    }
    result
}

/// `exit_group(status)`.
pub(crate) fn exit(status: i32) -> ! {
    // SAFETY: `exit_group` takes no pointer and does not return: the process
    // ends in the kernel, with nothing of this one left to run.
    unsafe {
        asm!(
            "svc #0",
            in("x8") EXIT_GROUP,
            in("x0") status.cast_unsigned() as usize,
            options(noreturn, nostack),
        );
    }
}
