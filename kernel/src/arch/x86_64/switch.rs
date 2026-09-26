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

use super::cpu;
use super::gdt;

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
/// pointer and `GS` base, its x87 and SSE state, and -- for a 32-bit program,
/// which addresses through segments -- its data segment selectors and the
/// three thread-local descriptors they may name (`docs/I386.md` §3.5).
///
/// The kernel is built for a target with no SSE and never touches any of
/// these, so a trap from ring 3 leaves them as the program had them. Two
/// programs taking turns need them saved and loaded by the scheduler whenever
/// it switches between tasks that run user code.
#[repr(C, align(16))]
#[derive(Debug, Clone)]
pub(crate) struct UserState {
    /// `FS_BASE`, which `arch_prctl(ARCH_SET_FS)` writes.
    thread_pointer: u64,
    /// The program's `GS_BASE`, which sits in `KERNEL_GS_BASE` while the
    /// kernel runs. Saved so that one program's base, whatever loaded it, is
    /// not the next one's.
    gs_base: u64,
    /// GDT slots 12 to 14: the thread-local descriptors `set_thread_area`
    /// installed, zero for none.
    tls: [u64; gdt::TLS_SLOTS],
    /// `DS`, `ES`, `FS` and `GS`, as the program left them.
    selectors: [u16; 4],
    /// The 512-byte `FXSAVE` area, at a sixteen-byte offset.
    fxsave: [u8; 512],
}

const _: () = assert!(
    core::mem::offset_of!(UserState, fxsave).is_multiple_of(16),
    "FXSAVE64 wants its area sixteen-byte aligned"
);

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

    /// Put `descriptor` in thread-local slot `index` of this saved state: a
    /// 32-bit program's `CLONE_SETTLS`, which names a `user_desc` for the
    /// child rather than a base (`docs/I386.md` §3.5). The child's `%gs`
    /// keeps its parent's selector and so reads through the new descriptor.
    /// False for an `index` past the three.
    pub(crate) fn set_thread_area(&mut self, index: usize, descriptor: u64) -> bool {
        self.tls
            .get_mut(index)
            .map(|slot| *slot = descriptor)
            .is_some()
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
            gs_base: 0,
            tls: [0; gdt::TLS_SLOTS],
            selectors: [0; 4],
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
    // SAFETY: the kernel side of `swapgs`, where the shadow is the program's.
    state.gs_base = unsafe { super::syscall::program_gs_base() };
    state.selectors = cpu::read_data_selectors();
    // SAFETY: the caller switches tasks with interrupts masked, so these are
    // this processor's slots and the outgoing thread's.
    state.tls = unsafe { gdt::read_tls() };
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
    // SAFETY: the caller switches with interrupts masked; the descriptors are
    // ones `set_thread_area` built, or zero.
    unsafe { gdt::write_tls(&state.tls) };
    // SAFETY: each selector checked loadable against the slots just written.
    unsafe {
        load_selectors(
            state.selectors,
            &state.tls,
            state.thread_pointer,
            state.gs_base,
        );
    }
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
    // `execve` empties the thread-local slots and every selector, as Linux's
    // `flush_thread` and `start_thread` do: the new image starts with none of
    // the old one's segments. A 32-bit one is given user data in `DS` and `ES`
    // as it is entered (`enter_compat_segments`).
    with_interrupts_masked(|| {
        // SAFETY: interrupts masked; zero descriptors.
        unsafe { gdt::write_tls(&fresh.tls) };
        // SAFETY: null selectors always load; the bases are zero, which is
        // valid.
        unsafe { load_selectors(fresh.selectors, &fresh.tls, 0, 0) };
    });
    // SAFETY: an area built by `UserState::new`, whose reserved bits are clear.
    unsafe { ferrix_fpu_restore(fresh.fxsave.as_ptr()) };
}

/// Load a program's four data selectors, each checked against `tls`, and
/// the `FS` and `GS` bases a null selector leaves to the MSRs: the thread
/// pointer `arch_prctl` set, and whatever `GS` base the program had.
///
/// # Safety
///
/// Interrupts masked, `tls` already in this processor's thread-local slots,
/// and both bases the program's own.
unsafe fn load_selectors(
    selectors: [u16; 4],
    tls: &[u64; gdt::TLS_SLOTS],
    fs_base: u64,
    gs_base: u64,
) {
    let [ds, es, fs, gs] = selectors.map(|selector| gdt::loadable(selector, tls));
    // SAFETY: each selector null or loadable, as `gdt::loadable` checked.
    unsafe { cpu::load_data_selectors(ds, es, fs) };
    if fs == 0 {
        // SAFETY: a user address the program set, or zero.
        unsafe { super::syscall::set_thread_pointer(fs_base) };
    }
    // SAFETY: as for the other three.
    unsafe { cpu::load_user_gs(gs) };
    if gs == 0 {
        // SAFETY: the program's own base, into the shadow it lives in while
        // the kernel runs.
        unsafe { super::syscall::set_program_gs_base(gs_base) };
    }
}

/// Load `selectors` -- `DS`, `ES`, `FS`, `GS` -- as a program's own, each
/// checked against this processor's thread-local slots, which are the
/// running thread's: what a 32-bit program's signal handler is entered with
/// and what its return from one puts back.
pub(crate) fn load_program_selectors(selectors: [u16; 4]) {
    with_interrupts_masked(|| {
        // SAFETY: interrupts masked, so these are the running thread's.
        let tls = unsafe { gdt::read_tls() };
        let [ds, es, fs, gs] = selectors.map(|selector| gdt::loadable(selector, &tls));
        // SAFETY: each checked loadable against the live slots.
        unsafe { cpu::load_data_selectors(ds, es, fs) };
        // SAFETY: as above.
        unsafe { cpu::load_user_gs(gs) };
    });
}

/// Give a 32-bit program user data in `DS` and `ES` as it is entered, from
/// its first instruction or after an `execve`: compatibility mode faults on a
/// null one, which is what `execve` and a new task leave. From then on the
/// program's own selectors travel with it (`save_user_state`).
///
/// `FS` stays null: both ways in come after `execve`'s reset or from a new
/// task's state, which leave it so.
pub(crate) fn enter_compat_segments() {
    let data = gdt::USER_DATA | 3;
    // SAFETY: user data is a present ring 3 data segment in every GDT this
    // kernel builds, and the null selector always loads.
    unsafe { cpu::load_data_selectors(data, data, 0) };
}

/// Install `descriptor` in the calling thread's thread-local slot `index`, or
/// in its first empty one when `index` is `None`, and answer the slot used:
/// `set_thread_area`'s half that is the processor's.
///
/// The slots are this processor's GDT's while the thread runs -- the switch
/// to it loaded them, and the switch away saves them -- so the write goes
/// there, with interrupts masked so the thread cannot move in between. A
/// data segment register naming the slot is reloaded, as Linux does, so the
/// program sees the new descriptor at once rather than at its next switch.
///
/// `None` when every slot is taken (`ESRCH` on Linux) or `index` is past
/// the three.
pub(crate) fn set_thread_area(index: Option<usize>, descriptor: u64) -> Option<usize> {
    with_interrupts_masked(|| {
        // SAFETY: interrupts masked, so these are the calling thread's.
        let mut tls = unsafe { gdt::read_tls() };
        let index = match index {
            Some(index) => index,
            None => tls.iter().position(|slot| *slot == 0)?,
        };
        *tls.get_mut(index)? = descriptor;
        // SAFETY: interrupts masked; `descriptor` is one `user_desc` built,
        // ring 3 data, or zero.
        unsafe { gdt::write_tls(&tls) };
        let named = |selector: u16| usize::from(selector >> 3) == gdt::TLS_FIRST_SLOT + index;
        let [ds, es, fs, gs] = cpu::read_data_selectors();
        if [ds, es, fs].into_iter().any(named) {
            let [ds, es, fs] = [ds, es, fs].map(|selector| gdt::loadable(selector, &tls));
            // SAFETY: each checked loadable against the slots just written.
            unsafe { cpu::load_data_selectors(ds, es, fs) };
        }
        if named(gs) {
            // SAFETY: as above.
            unsafe { cpu::load_user_gs(gdt::loadable(gs, &tls)) };
        }
        Some(index)
    })
}

/// The descriptor in the calling thread's thread-local slot `index`, zero
/// for an empty one: `get_thread_area`'s half that is the processor's.
pub(crate) fn thread_area(index: usize) -> Option<u64> {
    // SAFETY: interrupts masked by the closure's caller.
    with_interrupts_masked(|| unsafe { gdt::read_tls() }.get(index).copied())
}

/// Run `f` with interrupts masked, and put them back as they were.
fn with_interrupts_masked<R>(f: impl FnOnce() -> R) -> R {
    let open = cpu::read_rflags() & (1 << 9) != 0;
    cpu::disable_interrupts();
    let result = f();
    if open {
        cpu::enable_interrupts();
    }
    result
}
