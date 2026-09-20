//! ARMv7-A, in ARM state.
//!
//! The kernel's entry is `system_call` in `kernel/src/arch/armv7a/trap.rs`:
//! `svc #0`, the EABI's convention — the number in R7, the arguments in R0 to
//! R5, and the result back in R0. Every other register comes back as it went.
//! R7 is free to carry the number because the targets are built in ARM state,
//! where the frame pointer is R11; in Thumb state it would be R7.

use core::arch::{asm, naked_asm};

use ferrix_linux_abi::nr::arm::EXIT_GROUP;
use ferrix_native::Raw;

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
    trap(raw.number(), raw.args())
}

/// Make the Linux call `number` with `args`.
///
/// The same instruction and the same registers as [`call`]: the kernel picks
/// the ABI by the number's range and by nothing else (`dispatch` in
/// `kernel/src/syscall/mod.rs`), so a native program issues a Linux call
/// exactly as it issues one of its own. `exit` below has done this since this
/// file was written; `docs/CLIPBOARD.md` §5 is what made it worth naming.
///
/// # Safety
///
/// The caller is answerable for every pointer in `args`: each must be valid
/// for whatever the named call does with it, for the whole of the call. The
/// kernel checks that a pointer is the caller's before it touches it, so a
/// wrong one is refused rather than followed -- but a pointer to the wrong
/// *owned* memory is memory this program may see changed under it.
pub(crate) unsafe fn linux(number: usize, args: [usize; 6]) -> usize {
    trap(number, args)
}

/// The trap itself: a number, six argument registers, and the result.
///
/// One block for both ABIs, because it is one instruction and one register
/// assignment -- which is the whole of what `asm!` is here, and what
/// `scripts/asm-allowlist.json` admits.
fn trap(number: usize, args: [usize; 6]) -> usize {
    let [a0, a1, a2, a3, a4, a5] = args;
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
            in("r7") number,
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
