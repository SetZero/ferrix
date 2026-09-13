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
pub(crate) unsafe fn prepare_stack(
    top: u64,
    entry: extern "C" fn(usize) -> !,
    argument: usize,
) -> u64 {
    let frame: [u64; 7] = [
        0,                     // r15
        0,                     // r14
        argument as u64,       // r13
        entry as usize as u64, // r12
        0,                     // rbx
        0,                     // rbp
        ferrix_task_entry as *const () as usize as u64,
    ];
    let stack_pointer = top - FRAME_BYTES;
    // SAFETY: the caller guarantees the stack is mapped, writable and theirs,
    // and the frame is written entirely inside it.
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), stack_pointer as *mut u64, frame.len());
    };
    stack_pointer
}

/// What a program owns on this processor that no trap saves: its thread
/// pointer, and its x87 and SSE state.
///
/// The kernel is built for a target with no SSE and never touches either, so
/// a trap from ring 3 leaves them as the program had them. Two programs taking
/// turns need them saved and loaded by the scheduler whenever it switches
/// between tasks that run user code.
#[repr(C, align(16))]
#[derive(Debug, Clone)]
pub(crate) struct UserState {
    /// `FS_BASE`, which `arch_prctl(ARCH_SET_FS)` writes.
    thread_pointer: u64,
    /// Keeps the save area at a sixteen-byte offset.
    reserved: u64,
    /// The 512-byte `FXSAVE` area.
    fxsave: [u8; 512],
}

impl UserState {
    /// A copy of the user state this processor holds right now: what a fork
    /// child inherits.
    ///
    /// # Safety
    ///
    /// The registers must be the calling task's own, which they are inside its
    /// own system call.
    pub(crate) unsafe fn capture() -> UserState {
        let mut state = UserState::new();
        // SAFETY: the caller's guarantee.
        unsafe { save_user_state(&mut state) };
        state
    }

    /// Give the program `pointer` as its thread pointer, as `CLONE_SETTLS`
    /// asks.
    pub(crate) const fn set_thread_pointer(&mut self, pointer: u64) {
        self.thread_pointer = pointer;
    }

    /// A program's state before it has run: no thread pointer, and the x87 and
    /// SSE control words a processor has at reset.
    ///
    /// Not all zeros, and the difference is a crash: an all-zero `MXCSR`
    /// unmasks every SSE exception, so a program's first inexact division
    /// would take `#XM` instead of rounding. `0x1F80` masks them all, and
    /// `0x037F` does the same for the x87.
    pub(crate) const fn new() -> UserState {
        let mut fxsave = [0_u8; 512];
        let control = 0x037F_u16.to_le_bytes();
        fxsave[0] = control[0];
        fxsave[1] = control[1];
        let mxcsr = 0x1F80_u32.to_le_bytes();
        fxsave[24] = mxcsr[0];
        fxsave[25] = mxcsr[1];
        fxsave[26] = mxcsr[2];
        fxsave[27] = mxcsr[3];
        UserState {
            thread_pointer: 0,
            reserved: 0,
            fxsave,
        }
    }

    /// The 512-byte `FXSAVE` area: what a signal frame carries as `fpstate`.
    pub(super) const fn fxsave(&self) -> &[u8; 512] {
        &self.fxsave
    }

    /// The same area, for `rt_sigreturn` to fill from the frame.
    pub(super) const fn fxsave_mut(&mut self) -> &mut [u8; 512] {
        &mut self.fxsave
    }
}

/// Load `state`'s x87 and SSE registers and nothing else: not the thread
/// pointer, and not the entry stack. What `rt_sigreturn` puts back.
///
/// # Safety
///
/// The registers must be the calling task's own, and `state`'s `MXCSR` must
/// have no reserved bit set, which `FXRSTOR64` answers with `#GP` in ring 0.
pub(super) unsafe fn load_fpu(state: &UserState) {
    // SAFETY: a 512-byte area inside a sixteen-byte-aligned structure, whose
    // `MXCSR` the caller has masked.
    unsafe { ferrix_fpu_restore(state.fxsave.as_ptr()) };
}

global_asm!(
    r#"
.section .text

// void ferrix_fpu_save(u8 *area), area: 512 bytes
.globl ferrix_fpu_save
ferrix_fpu_save:
    fxsave64 (%rdi)
    retq

// void ferrix_fpu_restore(const u8 *area)
.globl ferrix_fpu_restore
ferrix_fpu_restore:
    fxrstor64 (%rdi)
    retq
"#,
    options(att_syntax)
);

unsafe extern "C" {
    /// `FXSAVE64` into `area`.
    fn ferrix_fpu_save(area: *mut u8);
    /// `FXRSTOR64` from `area`.
    fn ferrix_fpu_restore(area: *const u8);
}

/// Store the program state this processor holds into `state`.
///
/// # Safety
///
/// The registers must belong to the task `state` is for: it was the last task
/// with user state to run on this processor.
pub(crate) unsafe fn save_user_state(state: &mut UserState) {
    // SAFETY: reading `FS_BASE` has no side effects.
    state.thread_pointer = unsafe { super::syscall::thread_pointer() };
    // SAFETY: a 512-byte area inside a sixteen-byte-aligned structure, which
    // is what `FXSAVE64` writes.
    unsafe { ferrix_fpu_save(state.fxsave.as_mut_ptr()) };
}

/// Load `state` onto this processor for the task about to run, and point the
/// ways in from ring 3 at `entry_stack`.
///
/// # Safety
///
/// The task `state` belongs to must be the one this processor is switching to,
/// and `entry_stack` the top of its kernel stack.
pub(crate) unsafe fn restore_user_state(state: &UserState, entry_stack: u64) {
    // SAFETY: a user address the program set, or zero; nothing follows it here.
    unsafe { super::syscall::set_thread_pointer(state.thread_pointer) };
    // SAFETY: an area this module initialised or `FXSAVE64` wrote, so every
    // reserved bit `FXRSTOR64` checks is clear.
    unsafe { ferrix_fpu_restore(state.fxsave.as_ptr()) };
    // SAFETY: the caller guarantees the stack.
    unsafe { super::syscall::set_entry_stack(entry_stack) };
}

/// Put this processor's user state back to a program's starting state: no
/// thread pointer, reset floating-point control. What `execve` does to the
/// registers the old program left.
///
/// # Safety
///
/// Must be called by the user task whose registers these are, from inside its
/// own system call.
pub(crate) unsafe fn reset_user_state() {
    let fresh = UserState::new();
    // SAFETY: a zero thread pointer is always valid to hold.
    unsafe { super::syscall::set_thread_pointer(0) };
    // SAFETY: an area built by `UserState::new`, whose reserved bits are clear.
    unsafe { ferrix_fpu_restore(fresh.fxsave.as_ptr()) };
}
