//! The context switch.
//!
//! # Why there is assembly here
//!
//! A context switch is a function that returns onto a *different stack* from
//! the one it was called on. Rust has no way to say that: the callee-saved
//! registers it would restore on the way out belong to the caller it is about
//! to stop being, and the return address it would use lives on a stack that
//! is no longer current. So the whole of it is six pushes, one store, one
//! load, six pops and a return — and what makes it a switch rather than a
//! no-op is the two instructions in the middle.
//!
//! The System V ABI's callee-saved set is `rbx`, `rbp` and `r12` through
//! `r15`. Everything else is the caller's problem, and the caller here is
//! Rust, which has already spilled whatever it cared about. There is no
//! floating-point state to save: the kernel is built for a target with no
//! SSE, so it uses none.

use core::arch::global_asm;

/// Bytes the switch pushes: six registers and the return address.
const FRAME_BYTES: u64 = 7 * 8;

global_asm!(
    r#"
.section .text

// void ferrix_switch(u64 *save, u64 next)
//   rdi = where to write this context's stack pointer
//   rsi = the stack pointer to resume
.globl ferrix_switch
ferrix_switch:
    pushq %rbp
    pushq %rbx
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15
    movq  %rsp, (%rdi)
    movq  %rsi, %rsp
    popq  %r15
    popq  %r14
    popq  %r13
    popq  %r12
    popq  %rbx
    popq  %rbp
    retq

// Where a task starts the first time it is switched to: `prepare_stack` left
// its entry point in r12 and its argument in r13, and the stack is aligned as
// the ABI wants it at a call.
.globl ferrix_task_entry
ferrix_task_entry:
    movq %r13, %rdi
    callq *%r12
    ud2
"#,
    options(att_syntax)
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
/// The frame is what [`switch_to`] pops: six callee-saved registers and a
/// return address. Two of the registers carry the entry point and its
/// argument, because the trampoline the return address names is the only code
/// that runs before Rust does and it has nowhere else to read them from.
///
/// # Safety
///
/// `top` must be the top of a mapped, writable stack of at least
/// [`FRAME_BYTES`], owned by the caller and not in use.
pub(crate) unsafe fn prepare_stack(top: u64, entry: extern "C" fn(usize) -> !, argument: usize) -> u64 {
    let frame: [u64; 7] = [
        0,                       // r15
        0,                       // r14
        argument as u64,         // r13
        entry as usize as u64,   // r12
        0,                       // rbx
        0,                       // rbp
        ferrix_task_entry as usize as u64,
    ];
    let stack_pointer = top - FRAME_BYTES;
    // SAFETY: the caller guarantees the stack is mapped, writable and theirs,
    // and the frame is written entirely inside it.
    unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), stack_pointer as *mut u64, frame.len()) };
    stack_pointer
}
