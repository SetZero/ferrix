//! The context switch.
//!
//! # Why there is assembly here
//!
//! The same argument as x86-64's, in this architecture's registers: a
//! function that returns onto another stack cannot be written in Rust,
//! because the registers it would restore and the return address it would use
//! belong to the context it is in the middle of leaving.
//!
//! The AAPCS callee-saved set is `x19` through `x28`, the frame pointer `x29`
//! and the link register `x30`. There is no floating-point state to save: the
//! kernel is built for `aarch64-unknown-none-softfloat` and never touches the
//! SIMD registers, which is also why `d8`–`d15` — callee-saved on a target
//! that used them — do not appear here.

use core::arch::global_asm;

/// Bytes the switch pushes: twelve registers, which is also a multiple of the
/// sixteen `AArch64` requires of a stack pointer.
const FRAME_BYTES: u64 = 96;

global_asm!(
    r#"
.section .text

// void ferrix_switch(u64 *save, u64 next)
//   x0 = where to write this context's stack pointer
//   x1 = the stack pointer to resume
.globl ferrix_switch
ferrix_switch:
    stp x29, x30, [sp, #-96]!
    stp x27, x28, [sp, #16]
    stp x25, x26, [sp, #32]
    stp x23, x24, [sp, #48]
    stp x21, x22, [sp, #64]
    stp x19, x20, [sp, #80]
    mov x2, sp
    str x2, [x0]
    mov sp, x1
    ldp x19, x20, [sp, #80]
    ldp x21, x22, [sp, #64]
    ldp x23, x24, [sp, #48]
    ldp x25, x26, [sp, #32]
    ldp x27, x28, [sp, #16]
    ldp x29, x30, [sp], #96
    ret

// Where a task starts the first time it is switched to: `prepare_stack` left
// its entry point in x19 and its argument in x20.
.globl ferrix_task_entry
ferrix_task_entry:
    mov x0, x20
    blr x19
    brk #0
"#
);

unsafe extern "C" {
    /// Save this context and resume another. Declared here; defined above.
    fn ferrix_switch(save: *mut u64, next: u64);
    /// The first instruction a new task runs.
    fn ferrix_task_entry();
}

/// Stop running on this stack and continue on `next`, writing this context's
/// stack pointer to `save` so it can be resumed later.
///
/// # Safety
///
/// `save` must be the stack-pointer slot of the context calling this, and
/// `next` must be a stack pointer that [`prepare_stack`] produced or that an
/// earlier call to this function saved. No other processor may be running on
/// either stack, and both must stay mapped for as long as their contexts
/// exist.
pub(crate) unsafe fn switch_to(save: *mut u64, next: u64) {
    // SAFETY: the caller's guarantee is exactly the assembly's contract.
    unsafe { ferrix_switch(save, next) };
}

/// Lay out a stack so that switching to it calls `entry(argument)`.
///
/// # Safety
///
/// `top` must be the top of a mapped, writable stack of at least
/// [`FRAME_BYTES`], owned by the caller and not in use.
pub(crate) unsafe fn prepare_stack(
    top: u64,
    entry: extern "C" fn(usize) -> !,
    argument: usize,
) -> u64 {
    // Written out in the order the switch stores them, lowest address first,
    // so this reads against the `stp` sequence above rather than against a
    // list of indices. Two of the registers carry the entry point and its
    // argument, because the trampoline the link register names is the only
    // code that runs before Rust does and has nowhere else to read them from.
    let frame: [u64; 12] = [
        0,                                              // x29
        ferrix_task_entry as *const () as usize as u64, // x30, the link register
        0,                                              // x27
        0,                                              // x28
        0,                                              // x25
        0,                                              // x26
        0,                                              // x23
        0,                                              // x24
        0,                                              // x21
        0,                                              // x22
        entry as usize as u64,                          // x19
        argument as u64,                                // x20
    ];

    let stack_pointer = top - FRAME_BYTES;
    // SAFETY: the caller guarantees the stack is mapped, writable and theirs,
    // and the frame is written entirely inside it.
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), stack_pointer as *mut u64, frame.len());
    };
    stack_pointer
}
