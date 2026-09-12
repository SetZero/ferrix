//! Exception entry on `AArch64`.
//!
//! # Why there is assembly here
//!
//! The architecture defines a *table*, not a function: `VBAR_EL1` points at
//! sixteen entries at fixed 128-byte offsets, and the CPU jumps to the one that
//! matches what happened and where it came from. The layout is the interface —
//! a Rust array of function pointers is not what the hardware reads — and each
//! entry is entered with the interrupted program's registers still live.
//!
//! Sixteen entries, four for each of the four kinds of exception:
//!
//! | Offset | Taken from |
//! |---|---|
//! | `0x000` | the current EL, while `SP_EL0` is selected |
//! | `0x200` | the current EL, while `SP_ELx` is selected — where the kernel's own faults arrive |
//! | `0x400` | a lower EL running `AArch64` — where user mode's arrive |
//! | `0x600` | a lower EL running `AArch32`, which Ferrix never enters |
//!
//! and within each block, synchronous, `IRQ`, `FIQ`, `SError` in that order.
//!
//! Each entry is far too small for a register save, so it stashes `x0`/`x1`,
//! puts its own index in `x0` and branches to the common path.

use super::cpu;

/// Bytes in a saved frame. Must match [`TrapFrame`] exactly, and must be a
/// multiple of sixteen because `AArch64` faults on a misaligned stack pointer.
const FRAME_SIZE: usize = 304;

/// The register state at the point an exception was taken.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct TrapFrame {
    /// `x0` through `x30`. `x30` is the link register; there is no `x31`,
    /// because that encoding means the zero register or the stack pointer
    /// depending on the instruction.
    pub(crate) x: [u64; 31],
    /// `SP_EL0`: the user stack pointer when the exception came from EL0.
    pub(crate) sp: u64,
    /// Exception link register — where to return to.
    pub(crate) elr: u64,
    /// Saved program status.
    pub(crate) spsr: u64,
    /// Exception syndrome: what happened, and why.
    pub(crate) esr: u64,
    /// Fault address, for an abort.
    pub(crate) far: u64,
    /// Which of the sixteen vector entries was taken.
    pub(crate) kind: u64,
    /// Padding, so the frame is a multiple of sixteen bytes.
    pub(crate) reserved: u64,
}

impl TrapFrame {
    /// True if the exception came from EL0.
    ///
    /// Read from the vector entry rather than from `SPSR`: the four "lower EL"
    /// entries are, by definition, the only ones a user-mode exception can
    /// arrive through.
    pub(crate) const fn came_from_user(&self) -> bool {
        self.kind >= 8
    }

    /// The exception class: the top six bits of the syndrome.
    pub(crate) const fn exception_class(&self) -> u64 {
        self.esr >> 26
    }

    /// The instruction-specific syndrome: the bottom twenty-five bits.
    pub(crate) const fn syndrome(&self) -> u64 {
        self.esr & 0x01FF_FFFF
    }
}

// The frame layout is asserted against the assembly's fixed offsets below, so
// that adding a field without updating the save sequence is a build failure
// rather than a corrupted register.
const _: () = assert!(
    size_of::<TrapFrame>() == FRAME_SIZE,
    "the trap frame and the save sequence in the assembly must agree"
);
const _: () = assert!(
    FRAME_SIZE.is_multiple_of(16),
    "AArch64 requires a 16-byte aligned stack"
);

core::arch::global_asm!(
    r#"
.section .text

// One vector entry: stash x0 and x1, record which entry this is, and go.
// `.align 7` is 128 bytes, which is the spacing the architecture fixes.
.macro FERRIX_VECTOR index
.align 7
    sub  sp, sp, #304
    stp  x0, x1, [sp, #0]
    mov  x0, #\index
    b    ferrix_trap_save
.endm

.globl ferrix_vectors
.align 11
ferrix_vectors:
    FERRIX_VECTOR 0    // current EL, SP_EL0: synchronous
    FERRIX_VECTOR 1    // current EL, SP_EL0: IRQ
    FERRIX_VECTOR 2    // current EL, SP_EL0: FIQ
    FERRIX_VECTOR 3    // current EL, SP_EL0: SError
    FERRIX_VECTOR 4    // current EL, SP_ELx: synchronous
    FERRIX_VECTOR 5    // current EL, SP_ELx: IRQ
    FERRIX_VECTOR 6    // current EL, SP_ELx: FIQ
    FERRIX_VECTOR 7    // current EL, SP_ELx: SError
    FERRIX_VECTOR 8    // lower EL, AArch64: synchronous
    FERRIX_VECTOR 9    // lower EL, AArch64: IRQ
    FERRIX_VECTOR 10   // lower EL, AArch64: FIQ
    FERRIX_VECTOR 11   // lower EL, AArch64: SError
    FERRIX_VECTOR 12   // lower EL, AArch32: synchronous
    FERRIX_VECTOR 13   // lower EL, AArch32: IRQ
    FERRIX_VECTOR 14   // lower EL, AArch32: FIQ
    FERRIX_VECTOR 15   // lower EL, AArch32: SError

ferrix_trap_save:
    stp  x2, x3, [sp, #16]
    stp  x4, x5, [sp, #32]
    stp  x6, x7, [sp, #48]
    stp  x8, x9, [sp, #64]
    stp  x10, x11, [sp, #80]
    stp  x12, x13, [sp, #96]
    stp  x14, x15, [sp, #112]
    stp  x16, x17, [sp, #128]
    stp  x18, x19, [sp, #144]
    stp  x20, x21, [sp, #160]
    stp  x22, x23, [sp, #176]
    stp  x24, x25, [sp, #192]
    stp  x26, x27, [sp, #208]
    stp  x28, x29, [sp, #224]
    str  x30, [sp, #240]
    mrs  x9, sp_el0
    mrs  x10, elr_el1
    mrs  x11, spsr_el1
    mrs  x12, esr_el1
    mrs  x13, far_el1
    stp  x9, x10, [sp, #248]
    stp  x11, x12, [sp, #264]
    stp  x13, x0, [sp, #280]
    str  xzr, [sp, #296]
    mov  x0, sp
    bl   ferrix_trap_entry
    ldp  x9, x10, [sp, #248]
    ldr  x11, [sp, #264]
    msr  sp_el0, x9
    msr  elr_el1, x10
    msr  spsr_el1, x11
    ldp  x2, x3, [sp, #16]
    ldp  x4, x5, [sp, #32]
    ldp  x6, x7, [sp, #48]
    ldp  x8, x9, [sp, #64]
    ldp  x10, x11, [sp, #80]
    ldp  x12, x13, [sp, #96]
    ldp  x14, x15, [sp, #112]
    ldp  x16, x17, [sp, #128]
    ldp  x18, x19, [sp, #144]
    ldp  x20, x21, [sp, #160]
    ldp  x22, x23, [sp, #176]
    ldp  x24, x25, [sp, #192]
    ldp  x26, x27, [sp, #208]
    ldp  x28, x29, [sp, #224]
    ldr  x30, [sp, #240]
    ldp  x0, x1, [sp, #0]
    add  sp, sp, #304
    eret
"#
);

// Entering EL0, and coming back from it.
//
// The return half of the trap path above, in the other direction: that one
// comes *in* from EL0 through a vector and goes back with `eret`; this goes
// *out* with `eret` to a program that has never run, and returns only when that
// program's `exit_group` calls `ferrix_leave_user`. It mirrors x86-64's
// `ferrix_run_user` exactly, register for register, so the two can be read
// against each other.
core::arch::global_asm!(
    r#"
.section .text

// x0 = entry, x1 = user stack. Returns the exit status in x0, via leave_user.
.globl ferrix_run_user
.align 4
ferrix_run_user:
    // Nothing may interrupt the next few instructions: between parking the
    // stack pointer and the `eret`, this processor is on neither stack it will
    // end up on. IRQs stay masked in EL0 too -- see `USER_SPSR`.
    msr  daifset, #0xf

    // The callee-saved registers, which leave_user restores.
    stp  x19, x20, [sp, #-96]!
    stp  x21, x22, [sp, #16]
    stp  x23, x24, [sp, #32]
    stp  x25, x26, [sp, #48]
    stp  x27, x28, [sp, #64]
    stp  x29, x30, [sp, #80]

    // Park that stack pointer where leave_user will find it.
    mrs  x9, tpidr_el1
    mov  x10, sp
    str  x10, [x9, #{user_return}]

    // And move to the dedicated entry stack before dropping privilege. At EL1
    // `sp` *is* SP_EL1, the stack every exception from EL0 lands on, so this is
    // the separation x86-64 learned the hard way: had SP_EL1 stayed here, the
    // program's first demand fault would push its frame onto the stack holding
    // the registers parked above.
    ldr  x10, [x9, #{kernel_stack}]
    mov  sp, x10

    // The program's state.
    msr  sp_el0, x1
    msr  elr_el1, x0
    mov  x10, #{user_spsr}
    msr  spsr_el1, x10

    // Nothing of the kernel's survives into EL0.
    mov  x0, xzr
    mov  x1, xzr
    mov  x2, xzr
    mov  x3, xzr
    mov  x4, xzr
    mov  x5, xzr
    mov  x6, xzr
    mov  x7, xzr
    mov  x8, xzr
    mov  x9, xzr
    mov  x10, xzr
    mov  x11, xzr
    mov  x12, xzr
    mov  x13, xzr
    mov  x14, xzr
    mov  x15, xzr
    mov  x16, xzr
    mov  x17, xzr
    mov  x18, xzr
    mov  x19, xzr
    mov  x20, xzr
    mov  x21, xzr
    mov  x22, xzr
    mov  x23, xzr
    mov  x24, xzr
    mov  x25, xzr
    mov  x26, xzr
    mov  x27, xzr
    mov  x28, xzr
    mov  x29, xzr
    mov  x30, xzr
    eret

// x0 = exit status. Called from inside a system call made by a program
// ferrix_run_user started, on the same processor. Restoring the parked stack
// pointer abandons the system call's own frame on the entry stack, which is
// correct: the program it belonged to is gone.
.globl ferrix_leave_user
.align 4
ferrix_leave_user:
    mrs  x9, tpidr_el1
    ldr  x10, [x9, #{user_return}]
    mov  sp, x10
    ldp  x21, x22, [sp, #16]
    ldp  x23, x24, [sp, #32]
    ldp  x25, x26, [sp, #48]
    ldp  x27, x28, [sp, #64]
    ldp  x29, x30, [sp, #80]
    ldp  x19, x20, [sp], #96
    ret
"#,
    user_return = const USER_RETURN_OFFSET,
    kernel_stack = const KERNEL_STACK_OFFSET,
    user_spsr = const USER_SPSR,
);

/// Where the parked stack pointer lives in this processor's record.
const USER_RETURN_OFFSET: usize = core::mem::offset_of!(crate::smp::PerCpu, user_return);
/// Where the entry stack for exceptions from EL0 lives in the same record.
const KERNEL_STACK_OFFSET: usize = core::mem::offset_of!(crate::smp::PerCpu, kernel_stack);

/// `SPSR_EL1` for a program: `EL0t`, with debug, asynchronous aborts, IRQs and
/// FIQs all masked.
///
/// **IRQs masked on purpose, and not for long.** The program runs as a guest of
/// the boot task, whose address space field is `None`. A timer tick taken in
/// EL0 could switch to a task that has one, which installs that task's root
/// over the program's; switching back to the boot task is then `(Some, None)`
/// and *uninstalls* the root, and the handler returns into EL0 with nothing
/// mapped. Masking closes that deterministically. It goes when a program is a
/// scheduled task of its own rather than a guest of the boot task.
const USER_SPSR: u64 = (1 << 9) | (1 << 8) | (1 << 7) | (1 << 6);

unsafe extern "C" {
    /// Enter EL0 at `entry` on `stack`; returns when the program exits.
    fn ferrix_run_user(entry: u64, stack: u64) -> i32;
    /// Return from [`ferrix_run_user`] with `status`.
    fn ferrix_leave_user(status: i32) -> !;
}

/// Run a program at EL0 and return the status it exits with.
///
/// # Safety
///
/// A user address space must be installed on this processor, and `entry` and
/// `stack` must be addresses within it.
pub(crate) unsafe fn run_user(entry: u64, stack: u64) -> Result<i32, &'static str> {
    let entry_stack = crate::vmap::allocate_stack().map_err(|_| "no kernel stack for user mode")?;
    let Some(cpu) = crate::smp::this_cpu() else {
        return Err("no per-CPU record, so an exception from EL0 could not find a stack");
    };
    let at = (core::ptr::from_ref(cpu) as usize + KERNEL_STACK_OFFSET) as *mut u64;
    // SAFETY: this processor's own record, a `u64` field aligned by `repr(C)`.
    unsafe { at.write(entry_stack.top) };

    // SAFETY: the caller guarantees the address space and the stack.
    let status = unsafe { ferrix_run_user(entry, stack) };

    // SAFETY: the program is gone, so nothing is running on the entry stack.
    let _ = unsafe { crate::vmap::free_stack(entry_stack) };
    Ok(status)
}

/// Service a system call made from EL0 with `svc #0`.
///
/// The number is in `x8` and the arguments in `x0` to `x5`, and the result goes
/// back in `x0`. `ELR_EL1` already points past the `svc`, so returning resumes
/// the program at the next instruction with nothing to adjust — unlike a
/// breakpoint, which reports its own address.
///
/// `exit` and `exit_group` never reach `dispatch`: they leave EL0 here, before
/// anything else runs, the same as on x86-64. When a program is a scheduled task
/// rather than a guest of the boot task, `exit_group` becomes a teardown and can
/// move into `dispatch` with everything else.
///
/// # Errors
///
/// A system call from EL1, which is a kernel bug, or an `execve` this path does
/// not yet know how to honour.
pub(crate) fn system_call(frame: &mut TrapFrame) -> Result<(), &'static str> {
    use crate::syscall::{Outcome, SyscallArgs, dispatch};
    use ferrix_linux_abi::nr::Syscall;

    if !frame.came_from_user() {
        return Err("a system call from EL1");
    }
    let [x0, x1, x2, x3, x4, x5, _, _, x8, ..] = frame.x;
    let args = SyscallArgs {
        number: x8 as usize,
        args: [x0, x1, x2, x3, x4, x5],
    };

    if matches!(
        super::decode_syscall(args.number),
        Some(Syscall::Exit | Syscall::ExitGroup)
    ) {
        // SAFETY: this is a system call made by a program `run_user` started —
        // `came_from_user` above, and nothing else enters EL0 — so the parked
        // stack pointer and registers are where `ferrix_run_user` left them.
        unsafe { leave_user(x0 as i32) }
    }

    match dispatch(&args) {
        Outcome::Return(value) => {
            if let Some(result) = frame.x.first_mut() {
                *result = value as u64;
            }
            Ok(())
        }
        Outcome::Enter { .. } => Err("execve through the EL0 trap path is not wired yet"),
    }
}

/// Leave EL0, returning `status` from [`run_user`].
///
/// # Safety
///
/// Must be called from inside a system call made by a program [`run_user`]
/// started, on the same processor.
pub(crate) unsafe fn leave_user(status: i32) -> ! {
    // SAFETY: the caller guarantees the parked stack pointer and callee-saved
    // registers are still where `ferrix_run_user` put them.
    unsafe { ferrix_leave_user(status) }
}

unsafe extern "C" {
    /// The vector table, aligned as `VBAR_EL1` requires.
    static ferrix_vectors: [u8; 16 * 128];
}

/// Where the save sequence hands over.
///
/// Not called from Rust — the `bl` in the assembly above is its only caller.
#[unsafe(no_mangle)]
extern "C" fn ferrix_trap_entry(frame: &mut TrapFrame) {
    crate::trap::dispatch(frame);
}

/// Exception class: a data abort taken from a lower exception level.
const EC_DATA_ABORT_LOWER: u64 = 0b100100;
/// Exception class: a data abort taken from the current exception level.
const EC_DATA_ABORT_SAME: u64 = 0b100101;
/// Exception class: an instruction abort taken from a lower exception level.
const EC_INSTRUCTION_ABORT_LOWER: u64 = 0b100000;
/// Exception class: an instruction abort taken from the current level.
const EC_INSTRUCTION_ABORT_SAME: u64 = 0b100001;
/// Exception class: an `SVC` from `AArch64` — a system call.
const EC_SVC: u64 = 0b010101;
/// Exception class: a `BRK` instruction — a breakpoint.
const EC_BRK: u64 = 0b111100;
/// Exception class: the CPU could not classify the instruction.
const EC_UNKNOWN: u64 = 0b000000;

/// Data abort syndrome bit: the access was a write rather than a read.
const ISS_WRITE: u64 = 1 << 6;
/// Data and instruction abort syndrome field: the fault status code.
const ISS_FAULT_STATUS: u64 = 0b11_1111;

/// Mask selecting the *kind* of fault from a fault status code, leaving out
/// the two bits that say which level of the walk it happened at.
const FAULT_KIND: u64 = 0b111100;

/// `0b0011xx`: mapped, and the mapping refused the access.
///
/// The neighbouring encodings are `0b0001xx` for a translation fault, where
/// nothing was mapped, and `0b0010xx` for an access-flag fault, where something
/// was but had not been touched. Both mean "there is effectively nothing there"
/// as far as the fault handler is concerned, which is why only this one needs a
/// name: everything that is not a permission fault is treated as absent.
const FAULT_PERMISSION: u64 = 0b001100;

/// Vector entries 1, 5, 9 and 13 are `IRQ`.
const fn is_irq(kind: u64) -> bool {
    kind % 4 == 1
}

/// Turn an `AArch64` trap frame into the architecture-neutral description the
/// generic dispatcher works with.
pub(crate) fn classify(frame: &TrapFrame) -> crate::trap::Trap {
    use crate::trap::Trap;

    if is_irq(frame.kind) {
        // Unlike x86-64 there is no interrupt number in the exception itself:
        // which one fired has to be asked of the controller, and there is none
        // yet. Report the vector entry rather than inventing a number — this
        // classifies *what happened*, and what to do about it is the
        // dispatcher's decision, not this function's.
        return Trap::Interrupt(frame.kind as u32);
    }

    let class = frame.exception_class();
    match class {
        EC_BRK => Trap::Breakpoint,
        EC_SVC => Trap::SystemCall,
        EC_UNKNOWN => Trap::IllegalInstruction,
        EC_DATA_ABORT_LOWER
        | EC_DATA_ABORT_SAME
        | EC_INSTRUCTION_ABORT_LOWER
        | EC_INSTRUCTION_ABORT_SAME => Trap::PageFault(abort(frame, class)),
        _ => Trap::Fault {
            name: class_name(class),
            code: frame.esr,
        },
    }
}

/// Decode a data or instruction abort.
fn abort(frame: &TrapFrame, class: u64) -> crate::trap::PageFault {
    let data = class == EC_DATA_ABORT_LOWER || class == EC_DATA_ABORT_SAME;
    let status = frame.syndrome() & ISS_FAULT_STATUS;

    crate::trap::PageFault {
        address: frame.far,
        // The write bit only means anything for a *data* abort; on an
        // instruction abort that bit position is part of another field.
        write: data && frame.syndrome() & ISS_WRITE != 0,
        execute: !data,
        user: frame.came_from_user(),
        // A permission fault means a mapping existed and refused the access. A
        // translation or access-flag fault means there was effectively nothing
        // there, which is the case the demand-paging path handles.
        present: status & FAULT_KIND == FAULT_PERMISSION,
    }
}

/// A readable name for an exception class.
const fn class_name(class: u64) -> &'static str {
    match class {
        0b000000 => "unknown reason",
        0b000001 => "trapped WFI or WFE",
        0b000111 => "trapped SIMD or floating point",
        0b001110 => "illegal execution state",
        0b010101 => "supervisor call",
        0b011000 => "trapped system register access",
        0b100000 | 0b100001 => "instruction abort",
        0b100010 => "misaligned program counter",
        0b100100 | 0b100101 => "data abort",
        0b100110 => "stack pointer alignment fault",
        0b101100 => "floating point exception",
        0b101111 => "SError",
        0b111100 => "breakpoint instruction",
        _ => "exception",
    }
}

/// Print the interrupted state.
pub(crate) fn report_trap(frame: &TrapFrame) {
    use crate::console::println;

    println!(
        "  esr      {:#018x}  class {:#04x} ({})",
        frame.esr,
        frame.exception_class(),
        class_name(frame.exception_class())
    );
    println!("  far      {:#018x}", frame.far);
    println!("  elr      {:#018x}  spsr {:#018x}", frame.elr, frame.spsr);
    println!("  sp_el0   {:#018x}  vector entry {}", frame.sp, frame.kind);

    for pair in 0..15 {
        let low = frame.x.get(pair * 2).copied().unwrap_or(0);
        let high = frame.x.get(pair * 2 + 1).copied().unwrap_or(0);
        println!(
            "  x{:<2} {:#018x}  x{:<2} {:#018x}",
            pair * 2,
            low,
            pair * 2 + 1,
            high
        );
    }
    println!("  x30 {:#018x}", frame.x.get(30).copied().unwrap_or(0));
    println!(
        "  from     {}",
        if frame.came_from_user() {
            "EL0"
        } else {
            "the kernel"
        }
    );
}

/// Install the vector table on this core.
///
/// # Safety
///
/// Must be called on every core, once, before anything on it can fault and
/// before it unmasks interrupts. One table serves every core: it holds code,
/// and no per-core state.
pub(crate) unsafe fn init() {
    let table = (&raw const ferrix_vectors) as u64;
    // SAFETY: `table` is the vector table in this image, 2048-byte aligned as
    // `VBAR_EL1` requires, and every entry branches to a real save sequence.
    unsafe { cpu::write_vbar(table) };
}

/// Raise a breakpoint, so the boot self-check can prove the trap path runs.
pub(crate) fn breakpoint() {
    // SAFETY: `brk` raises a synchronous exception the vector table handles.
    // Unlike x86-64's `int3`, the link register points *at* this instruction
    // rather than past it, which is why returning needs `advance_past_breakpoint`.
    unsafe {
        core::arch::asm!("brk #0", options(nomem, nostack));
    }
}

/// Step the return address over a breakpoint.
///
/// `AArch64` and x86-64 differ here and the difference is easy to miss: `int3`
/// leaves the saved instruction pointer *after* the trap, so returning
/// continues; `brk` leaves it *on* the instruction, so returning re-executes it
/// forever. Every `AArch64` instruction is four bytes.
pub(crate) const fn advance_past_breakpoint(frame: &mut TrapFrame) {
    frame.elr += 4;
}
