//! x86-64.
//!
//! The kernel's entry is `ferrix_syscall_stub` in
//! `kernel/src/arch/x86_64/syscall.rs`: `SYSCALL`, the number in RAX, the
//! arguments in RDI, RSI, RDX, R10, R8 and R9 — R10, not RCX, because the
//! instruction overwrites RCX with the return address and R11 with the flags —
//! and the result back in RAX. Every other register comes back as it went.

use core::arch::{asm, naked_asm};

use ferrix_linux_abi::nr::x86_64::EXIT_GROUP;
use ferrix_native::Raw;

/// The process's first instruction.
///
/// `ferrix_enter_user` starts a program with `SYSRET` and the stack pointer a
/// multiple of 16. A function expects to be entered by a `call`, eight bytes
/// below that; so the stack is aligned and `ferrix_rt_start` is called, which
/// pushes the return address the ABI wants. RBP is zeroed so a frame-pointer
/// walk ends here. The bootstrap handle's register, RDI, is already the first
/// argument.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub(crate) extern "C" fn _start() -> ! {
    naked_asm!(
        "xor ebp, ebp",
        "and rsp, -16",
        "call ferrix_rt_start",
        "ud2",
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
    // The instruction clobbers RCX and R11, declared, and no stack.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number => result,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            in("r9") a5,
            lateout("rcx") _,
            lateout("r11") _,
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
            "syscall",
            in("rax") EXIT_GROUP,
            in("rdi") status.cast_unsigned() as usize,
            options(noreturn, nostack),
        );
    }
}
