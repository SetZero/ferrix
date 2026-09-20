//! ARMv7-A, in ARM state.
//!
//! The kernel's entry is `system_call` in `kernel/src/arch/armv7a/trap.rs`:
//! `svc #0`, the EABI's convention — the number in R7, the arguments in R0 to
//! R5, and the result back in R0. Every other register comes back as it went.
//! R7 is free to carry the number because the targets are built in ARM state,
//! where the frame pointer is R11; in Thumb state it would be R7.

use core::arch::{asm, naked_asm};

use ferrix_linux_abi::nr::arm as nr;
use ferrix_linux_abi::nr::arm::EXIT_GROUP;
use ferrix_native::Raw;
use ferrix_native::linux::Numbers;

/// The process's first instruction.
///
/// `ferrix_enter_user` starts a program with `rfeia`, which leaves the frame
/// pointer and link register as whatever it put in them rather than a caller's
/// frame. They are zeroed so a frame-pointer walk ends here, the stack pointer
/// is brought to the multiple of 8 the AAPCS requires at a call, and
/// `ferrix_rt_start` is called. The bootstrap handle's register, R0, is
/// already the first argument.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub(crate) extern "C" fn _start() -> ! {
    naked_asm!(
        "mov r11, #0",
        "mov lr, #0",
        "bic sp, sp, #7",
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
    // The kernel preserves every register but R0, and uses no user stack.
    unsafe {
        asm!(
            "svc #0",
            in("r7") raw.number(),
            inlateout("r0") a0 => result,
            in("r1") a1,
            in("r2") a2,
            in("r3") a3,
            in("r4") a4,
            in("r5") a5,
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
            in("r7") EXIT_GROUP,
            in("r0") status.cast_unsigned() as usize,
            options(noreturn, nostack),
        );
    }
}

/// This architecture's numbers for the Linux calls `ferrix_native::linux`
/// makes, which that crate may not choose for itself: `libs/*` is
/// architecture-neutral and this directory is the facade.
pub(crate) const LINUX_NUMBERS: Numbers = Numbers {
    read: nr::READ,
    write: nr::WRITE,
    close: nr::CLOSE,
    socket: nr::SOCKET,
    bind: nr::BIND,
    listen: nr::LISTEN,
    accept4: nr::ACCEPT4,
    unlinkat: nr::UNLINKAT,
    clock_gettime: nr::CLOCK_GETTIME,
};
