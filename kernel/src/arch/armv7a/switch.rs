//! The context switch.
//!
//! # Why there is assembly here
//!
//! The same argument as the other two architectures': a function that returns
//! onto another stack cannot be written in Rust. What is particular to this
//! one is which stack. Every exception on ARMv7-A is taken into a mode with
//! its own banked stack pointer, and `kernel/src/arch/armv7a/trap.rs` answers
//! that by keeping everything on the SVC-mode stack — so a task's context is
//! its SVC stack pointer, and this saves and restores exactly that.
//!
//! The AAPCS callee-saved set is `r4` through `r11` and the link register. A
//! ninth register is pushed with them as padding, because nine words would
//! leave the stack four-byte aligned and the ABI requires eight at a call.
//! There is no floating-point state: the kernel is soft-float throughout.

use core::arch::global_asm;

/// Bytes the switch pushes: nine registers and one word of padding.
const FRAME_BYTES: u32 = 40;

global_asm!(
    r#"
.arm
.section .text

// void ferrix_switch(u32 *save, u32 next)
//   r0 = where to write this context's stack pointer
//   r1 = the stack pointer to resume
.globl ferrix_switch
ferrix_switch:
    push {{r4-r11, lr}}
    sub  sp, sp, #4
    str  sp, [r0]
    mov  sp, r1
    add  sp, sp, #4
    pop  {{r4-r11, pc}}

// Where a task starts the first time it is switched to: `prepare_stack` left
// its entry point in r4 and its argument in r5.
.globl ferrix_task_entry
ferrix_task_entry:
    mov r0, r5
    blx r4
    udf #0
"#
);

unsafe extern "C" {
    /// Save this context and resume another. Declared here; defined above.
    fn ferrix_switch(save: *mut u32, next: u32);
    /// The first instruction a new task runs.
    fn ferrix_task_entry();
}

/// Stop running on this stack and continue on `next`, writing this context's
/// stack pointer to `save` so it can be resumed later.
///
/// The facade's addresses are 64-bit on every architecture; here they are
/// narrowed to the 32 bits an address actually has, which loses nothing.
///
/// # Safety
///
/// `save` must be the stack-pointer slot of the context calling this, and
/// `next` must be a stack pointer that [`prepare_stack`] produced or that an
/// earlier call to this function saved. No other processor may be running on
/// either stack, and both must stay mapped for as long as their contexts
/// exist.
pub(crate) unsafe fn switch_to(save: *mut u64, next: u64) {
    // SAFETY: the caller's guarantee is the assembly's contract. The slot is
    // a `u64` the kernel keeps for every architecture; on this one only its
    // low half is used, and the assembly writes exactly that half — so the
    // pointer is cast rather than the value, and the high half stays zero.
    unsafe { ferrix_switch(save.cast::<u32>(), next as u32) };
}

/// Lay out a stack so that switching to it calls `entry(argument)`.
///
/// # Safety
///
/// `top` must be the top of a mapped, writable stack of at least
/// [`FRAME_BYTES`], owned by the caller and not in use.
pub(crate) unsafe fn prepare_stack(top: u64, entry: extern "C" fn(usize) -> !, argument: usize) -> u64 {
    // Lowest address first: the padding word, then r4 through r11, then the
    // link register the `pop` loads into the program counter.
    let mut frame = [0u32; 10];
    let entry_slot = frame.get_mut(1);
    let argument_slot = frame.get_mut(2);
    let return_slot = frame.get_mut(9);
    if let (Some(entry_slot), Some(argument_slot), Some(return_slot)) =
        (entry_slot, argument_slot, return_slot)
    {
        *entry_slot = entry as usize as u32;
        *argument_slot = argument as u32;
        *return_slot = ferrix_task_entry as usize as u32;
    }

    let stack_pointer = (top as u32) - FRAME_BYTES;
    // SAFETY: the caller guarantees the stack is mapped, writable and theirs,
    // and the frame is written entirely inside it.
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), stack_pointer as *mut u32, frame.len());
    };
    u64::from(stack_pointer)
}
