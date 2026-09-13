//! The paranoid entries: the NMI, the debug exception, the machine check and
//! the double fault.
//!
//! # Why these four cannot use the ordinary entry
//!
//! The ordinary stub decides whether to `swapgs` from the saved `CS`: ring 3
//! means `GS` is the program's, ring 0 the kernel's. That holds for everything
//! the kernel can hold off. An interrupt waits for `sti`, and every stretch of
//! ring 0 that runs with the program's `GS` or stack runs under `cli`; a fault
//! comes from an instruction, and those stretches hold none that fault.
//!
//! These four hold off for nothing. The `SYSCALL` trampoline has four ring-0
//! states in which the saved `CS` lies about `GS`, or the stack is the
//! program's:
//!
//! ```text
//!   ferrix_syscall_stub:  swapgs                 GS program's, RSP program's
//!                         movq %rsp, %gs:16      GS kernel's,  RSP program's
//!                         movq %gs:8, %rsp
//!   ...
//!                         popq %rsp
//!                         swapgs                 GS kernel's,  RSP program's
//!                         sysretq                GS program's, RSP program's
//! ```
//!
//! and `ferrix_enter_user`, `ferrix_resume_user` and the trap stub's own return
//! have the same shape. An NMI or a hardware breakpoint there, decided by `CS`,
//! runs its handler through a per-CPU pointer the program chose.
//!
//! # What the entry does
//!
//! `ferrix_paranoid_common` in `trap.rs` builds the ordinary frame on the
//! vector's own IST stack (`gdt.rs`), then:
//!
//! * **`GS` from `GS_BASE`.** It reads the register: a kernel `GS` base is a
//!   per-CPU record, a heap address in the upper half with its sign bit set, and
//!   no program can hold one -- a program starts with a `GS` base of zero
//!   (`syscall::init`), there is no `ARCH_SET_GS` and no `FSGSBASE`, and
//!   loading a selector in long mode sets a 32-bit base. Anything else is the
//!   program's, so it swaps, and remembers in `EBX` to swap back on the way out.
//!   Linux's `paranoid_entry` makes the same decision the same way. An
//!   `ARCH_SET_GS`, when one is written, must refuse a kernel address, as
//!   `ARCH_SET_FS` does, or this rule stops holding.
//! * **`DR7` saved and cleared** for the handler's whole run, and restored on
//!   the way out.
//! * **Each stack counts its occupants** in the word above the frame (the IST
//!   entry is sixteen bytes below the top). A second is reported, as FX-9006,
//!   rather than returned from.
//! * **A `#DB` from ring 3 leaves the IST stack.** It is the program's own --
//!   a trap flag, `SIGTRAP` -- and delivering it may block, switch or end the
//!   task, none of which can happen on a stack the next `#DB` on this processor
//!   starts from. Its frame is copied to the task's kernel stack, where the
//!   processor would have pushed it without the IST, and takes the ordinary
//!   path, whose `CS` test is right for a ring-3 frame.
//!
//! # What the handler does
//!
//! For now, stops the machine, on every vector: the machine check as FX-9005,
//! the rest as FX-9004. What this entry changes is where that report is made
//! -- on a stack of its own and through the kernel's `GS`, wherever the
//! processor was.
//!
//! # Nesting
//!
//! A handler on an IST stack that is entered again from its own vector starts
//! at the same top and overwrites itself. The scheme is to make that
//! impossible, and to catch it if a rule below is broken.
//!
//! * **`#DB` cannot nest.** The interrupt gate clears `TF`, so a handler does
//!   not single-step itself. `DR7` is cleared before any handler code runs,
//!   so no breakpoint fires while one is on its stack -- and an NMI that
//!   arrives in between clears it for its own run too. What is left is a
//!   breakpoint on the entry's own instructions before the clear, or on the IST
//!   stacks: nothing may arm one there, and an interface that lets anything
//!   arm breakpoints must refuse `ferrix_paranoid_stubs` and the stacks, as
//!   Linux refuses `noinstr` text. This is Linux's scheme since 5.10, which
//!   retired its IST shift for it.
//! * **An NMI cannot nest.** The processor delivers no NMI after one until the
//!   next `iretq`, and the handler's own is its last instruction. An exception
//!   inside the handler that returned would be an earlier `iretq`, so a
//!   handler that returns must take none.
//! * **`#MC` and `#DF` never return**, and a second on the same stack is the
//!   report failing.

use super::cpu;
use super::trap::TrapFrame;
use crate::panic::catalog;

/// `#MC`.
const MACHINE_CHECK: u64 = 18;

/// `DR6` with no condition recorded: the fixed-one bits set, the rest clear.
/// Linux's `DR6_RESERVED`.
const DR6_CLEAR: u64 = 0xFFFE_0FF0;

/// Where `ferrix_paranoid_common` hands over, on the vector's own stack, with
/// the kernel's `GS` and `DR7` clear.
///
/// `occupants` is the count of exceptions on this stack, this one included.
#[unsafe(no_mangle)]
extern "C" fn ferrix_paranoid_entry(frame: &mut TrapFrame, occupants: u64) {
    if occupants != 1 {
        crate::trap::fatal(
            frame,
            "an exception nested on its own interrupt stack",
            &catalog::NESTED_INTERRUPT_STACK,
        );
    }
    match frame.vector {
        MACHINE_CHECK => crate::trap::fatal(frame, "machine check", &catalog::MACHINE_CHECK),
        vector => crate::trap::fatal(
            frame,
            super::trap::vector_name(vector),
            &catalog::UNEXPECTED_EXCEPTION,
        ),
    }
}

/// Read and clear `DR6`. Sticky, so a `#DB` that left it set would be read
/// again by the next one.
pub(super) fn take_debug_status() -> u64 {
    let status = cpu::read_dr6();
    // SAFETY: the value DR6 holds with nothing recorded, reserved bits as the
    // processor defines them.
    unsafe { cpu::write_dr6(DR6_CLEAR) };
    status
}
