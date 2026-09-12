//! Exception entry on ARMv7-A.
//!
//! # Why there is assembly here
//!
//! As on AArch64 the architecture defines a *table*, not a function: `VBAR`
//! points at eight entries of one instruction each, and the CPU branches to
//! the one that matches what happened. What is different is where it arrives.
//! An AArch64 exception lands at EL1 on the kernel's own stack; an ARMv7-A one
//! lands in one of five processor modes, each with a banked stack pointer and
//! link register of its own and the interrupted program's registers still live.
//!
//! The kernel gives none of those modes a stack. Each stub corrects its mode's
//! link register to the instruction the exception interrupted — the offset
//! is the architecture's and differs per exception — and then `srsdb` stores
//! that return address and the saved status *on the SVC-mode stack*, and
//! `cps` switches to SVC mode. From there it is one common path, on the stack
//! the kernel was already running on, ending in `rfeia`, which loads the
//! program counter and the status from the same two words in one instruction.
//! This is the construct `srs` and `rfe` exist for, and it is why no mode but
//! SVC ever needs a stack.
//!
//! | Offset | Exception | Return address is |
//! |---|---|---|
//! | `0x00` | reset | never taken through `VBAR` |
//! | `0x04` | undefined instruction | `lr - 4`, the instruction |
//! | `0x08` | supervisor call | `lr`, the one after it |
//! | `0x0C` | prefetch abort, including `bkpt` | `lr - 4`, the instruction |
//! | `0x10` | data abort | `lr - 8`, the instruction, so it retries |
//! | `0x14` | hypervisor trap | never taken below HYP |
//! | `0x18` | IRQ | `lr - 4`, the instruction interrupted |
//! | `0x1C` | FIQ | `lr - 4` |

use super::cpu;

/// Bytes in a saved frame. Must match [`TrapFrame`] exactly, and a multiple of
/// eight, which is what the AAPCS requires of the stack at a call.
const FRAME_SIZE: usize = 80;

/// The register state at the point an exception was taken, in the order the
/// save path leaves it on the stack: lowest address first.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct TrapFrame {
    /// Which of the eight vector entries was taken.
    pub(crate) kind: u32,
    /// The fault status register, for an abort: `DFSR` or `IFSR`.
    pub(crate) fsr: u32,
    /// The fault address register, for an abort: `DFAR` or `IFAR`.
    pub(crate) far: u32,
    /// Padding, so the frame keeps the stack eight-byte aligned.
    pub(crate) reserved: u32,
    /// `r0` through `r12`.
    pub(crate) r: [u32; 13],
    /// The interrupted code's link register — its SVC-mode one.
    pub(crate) lr: u32,
    /// Where to return to: the interrupted instruction, or for a system call
    /// the one after it.
    pub(crate) pc: u32,
    /// The interrupted code's program status.
    pub(crate) cpsr: u32,
}

impl TrapFrame {
    /// True if the exception came from user mode.
    pub(crate) const fn came_from_user(&self) -> bool {
        self.cpsr & 0x1F == MODE_USR
    }
}

// The frame layout is asserted against the assembly's fixed offsets, so that
// adding a field without updating the save sequence is a build failure rather
// than a corrupted register.
const _: () = assert!(
    size_of::<TrapFrame>() == FRAME_SIZE,
    "the trap frame and the save sequence in the assembly must agree"
);
const _: () = assert!(
    FRAME_SIZE.is_multiple_of(8),
    "the AAPCS requires an eight-byte aligned stack at a call"
);

/// `CPSR.M` for user mode.
const MODE_USR: u32 = 0x10;

/// Vector entries, as the stubs number them.
const KIND_RESET: u32 = 0;
const KIND_UNDEFINED: u32 = 1;
const KIND_SVC: u32 = 2;
const KIND_PREFETCH_ABORT: u32 = 3;
const KIND_DATA_ABORT: u32 = 4;
const KIND_HYP_TRAP: u32 = 5;
const KIND_IRQ: u32 = 6;
const KIND_FIQ: u32 = 7;

core::arch::global_asm!(
    r#"
.arm
.section .text.vectors, "ax"

// The table itself: one branch per exception, 32-byte aligned as VBAR requires.
.balign 32
.globl ferrix_vectors
ferrix_vectors:
    b ferrix_stub_reset
    b ferrix_stub_undefined
    b ferrix_stub_svc
    b ferrix_stub_prefetch_abort
    b ferrix_stub_data_abort
    b ferrix_stub_hyp_trap
    b ferrix_stub_irq
    b ferrix_stub_fiq

// Each stub: correct the banked link register to the return address, store it
// and the banked saved status on the SVC stack, move to SVC mode, save the
// general registers, and pass the common path the kind of exception in r0 and
// the fault status and address in r1 and r2. r0 to r3 are free to use once the
// push has saved them.
ferrix_stub_reset:
    srsdb sp!, #0x13
    cps   #0x13
    push  {{r0-r12, lr}}
    mov   r0, #0
    mov   r1, #0
    mov   r2, #0
    b     ferrix_trap_common

ferrix_stub_undefined:
    sub   lr, lr, #4
    srsdb sp!, #0x13
    cps   #0x13
    push  {{r0-r12, lr}}
    mov   r0, #1
    mov   r1, #0
    mov   r2, #0
    b     ferrix_trap_common

ferrix_stub_svc:
    srsdb sp!, #0x13
    cps   #0x13
    push  {{r0-r12, lr}}
    mov   r0, #2
    mov   r1, #0
    mov   r2, #0
    b     ferrix_trap_common

ferrix_stub_prefetch_abort:
    sub   lr, lr, #4
    srsdb sp!, #0x13
    cps   #0x13
    push  {{r0-r12, lr}}
    mov   r0, #3
    mrc   p15, 0, r1, c5, c0, 1
    mrc   p15, 0, r2, c6, c0, 2
    b     ferrix_trap_common

ferrix_stub_data_abort:
    sub   lr, lr, #8
    srsdb sp!, #0x13
    cps   #0x13
    push  {{r0-r12, lr}}
    mov   r0, #4
    mrc   p15, 0, r1, c5, c0, 0
    mrc   p15, 0, r2, c6, c0, 0
    b     ferrix_trap_common

ferrix_stub_hyp_trap:
    srsdb sp!, #0x13
    cps   #0x13
    push  {{r0-r12, lr}}
    mov   r0, #5
    mov   r1, #0
    mov   r2, #0
    b     ferrix_trap_common

ferrix_stub_irq:
    sub   lr, lr, #4
    srsdb sp!, #0x13
    cps   #0x13
    push  {{r0-r12, lr}}
    mov   r0, #6
    mov   r1, #0
    mov   r2, #0
    b     ferrix_trap_common

ferrix_stub_fiq:
    sub   lr, lr, #4
    srsdb sp!, #0x13
    cps   #0x13
    push  {{r0-r12, lr}}
    mov   r0, #7
    mov   r1, #0
    mov   r2, #0
    b     ferrix_trap_common

// The common path. The frame is complete once kind, status, address and a
// padding word are pushed below the registers. The stack an exception
// interrupted is only four-byte aligned in general, so it is aligned to eight
// for the call, and r4 and r5 -- saved above, and preserved by the callee --
// remember the frame and the adjustment.
ferrix_trap_common:
    mov   r3, #0
    push  {{r0-r3}}
    mov   r4, sp
    and   r5, sp, #4
    sub   sp, sp, r5
    mov   r0, r4
    bl    ferrix_trap_entry
    add   sp, sp, r5
    add   sp, sp, #16
    pop   {{r0-r12, lr}}
    rfeia sp!
"#
);

// Entering USR mode, and coming back from it.
//
// AArch64's `ferrix_run_user` in coprocessor 15's spelling, and the return half
// of the vector path above run backwards: that one stores a return address and
// status with `srsdb` and leaves with `rfeia`; this builds a return address and
// status for a program that has never run and leaves with the same `rfeia`.
core::arch::global_asm!(
    r#"
.section .text
.arm

// r0 = entry, r1 = user stack, r2 = the program's CPSR. Returns the exit
// status in r0, via leave_user.
.globl ferrix_run_user
.balign 4
ferrix_run_user:
    // Nothing may interrupt this: between parking the stack pointer and the
    // `rfeia`, this processor is on neither stack it will end up on. IRQs and
    // FIQs stay masked in USR mode too -- see `USER_CPSR`.
    cpsid if

    // The callee-saved registers, which leave_user restores. Ten words keeps
    // the stack eight-byte aligned, as the EABI requires at a call.
    push  {{r4-r12, lr}}

    // Park that stack pointer where leave_user will find it.
    mrc   p15, 0, r9, c13, c0, 4
    mov   r10, sp
    str   r10, [r9, #{user_return}]

    // The program's stack pointer is banked, and System mode shares USR's, so
    // it is set from there without ever running in USR with a wrong one.
    cps   #0x1f
    mov   sp, r1
    mov   lr, #0
    cps   #0x13

    // The dedicated entry stack. Every exception from USR is stored with
    // `srsdb` onto the SVC stack, so this is the separation AArch64 needs for
    // SP_EL1: had SVC's sp stayed here, the program's first fault would push
    // its frame over the registers parked above.
    ldr   r10, [r9, #{kernel_stack}]
    mov   sp, r10

    // What `rfeia` takes: the program's first instruction, then its status.
    sub   sp, sp, #8
    str   r0, [sp]
    str   r2, [sp, #4]

    // Nothing of the kernel's survives into USR mode.
    mov   r0, #0
    mov   r1, #0
    mov   r2, #0
    mov   r3, #0
    mov   r4, #0
    mov   r5, #0
    mov   r6, #0
    mov   r7, #0
    mov   r8, #0
    mov   r9, #0
    mov   r10, #0
    mov   r11, #0
    mov   r12, #0
    rfeia sp!

// r0 = exit status. Called from inside a system call made by a program
// ferrix_run_user started, on the same processor.
.globl ferrix_leave_user
.balign 4
ferrix_leave_user:
    mrc   p15, 0, r9, c13, c0, 4
    ldr   r10, [r9, #{user_return}]
    mov   sp, r10
    pop   {{r4-r12, pc}}
"#,
    user_return = const USER_RETURN_OFFSET,
    kernel_stack = const KERNEL_STACK_OFFSET,
);

/// Where the parked stack pointer lives in this processor's record. The field
/// is a `u64` and an address here is 32 bits, so the assembly reads and writes
/// its low word, which on a little-endian machine is the one at this offset.
const USER_RETURN_OFFSET: usize = core::mem::offset_of!(crate::smp::PerCpu, user_return);
/// Where the entry stack for exceptions from USR mode lives in the same record.
const KERNEL_STACK_OFFSET: usize = core::mem::offset_of!(crate::smp::PerCpu, kernel_stack);

/// `CPSR` for a program: USR mode, little-endian, with asynchronous aborts,
/// IRQs and FIQs masked. ARM state, unless the entry point says Thumb -- see
/// [`CPSR_THUMB`].
///
/// IRQs masked for AArch64's reason, and just as temporarily: a program is a
/// guest of the boot task, whose address space is `None`, so a tick taken in
/// USR mode could switch to a task with a space and back again, and the switch
/// back uninstalls the program's root.
const USER_CPSR: u32 = MODE_USR | (1 << 8) | (1 << 7) | (1 << 6);

/// `CPSR.T`: execute Thumb instructions.
///
/// Set when the entry point's bit 0 is, which is the interworking convention an
/// ELF follows and the one Linux's `start_thread` honours. Toolchains building
/// for ARMv7-A default to Thumb-2, so this is the common case rather than an
/// exotic one: Alpine's busybox enters at an odd address, and entered in ARM
/// state its first Thumb instructions decode as undefined three words in.
const CPSR_THUMB: u32 = 1 << 5;

unsafe extern "C" {
    /// Enter USR mode at `entry` on `stack` with `cpsr`; returns when the
    /// program exits.
    fn ferrix_run_user(entry: u32, stack: u32, cpsr: u32) -> i32;
    /// Return from [`ferrix_run_user`] with `status`.
    fn ferrix_leave_user(status: i32) -> !;
}

/// Run a program in USR mode and return the status it exits with.
///
/// # Safety
///
/// A user address space must be installed on this processor, and `entry` and
/// `stack` must be addresses within it.
pub(crate) unsafe fn run_user(entry: u64, stack: u64) -> Result<i32, &'static str> {
    let entry = u32::try_from(entry).map_err(|_| "a program entry point above 4 GiB")?;
    let (entry, cpsr) = if entry & 1 == 0 {
        (entry, USER_CPSR)
    } else {
        (entry & !1, USER_CPSR | CPSR_THUMB)
    };
    let stack = u32::try_from(stack).map_err(|_| "a program stack above 4 GiB")?;
    let entry_stack = crate::vmap::allocate_stack().map_err(|_| "no kernel stack for user mode")?;
    let Some(cpu) = crate::smp::this_cpu() else {
        return Err("no per-CPU record, so an exception from USR mode could not find a stack");
    };
    let at = (core::ptr::from_ref(cpu) as usize + KERNEL_STACK_OFFSET) as *mut u64;
    // SAFETY: this processor's own record, a `u64` field aligned by `repr(C)`.
    unsafe { at.write(entry_stack.top) };

    // A program starts with no thread pointer, not the last one's. musl sets
    // its own with `set_tls` before it reads it, but a program that read first
    // would otherwise be handed an address in somebody else's memory.
    cpu::write_tpidruro(0);

    // SAFETY: the caller guarantees the address space and the stack.
    let status = unsafe { ferrix_run_user(entry, stack, cpsr) };

    // SAFETY: the program is gone, so nothing is running on the entry stack.
    let _ = unsafe { crate::vmap::free_stack(entry_stack) };
    Ok(status)
}

/// Service a system call made from USR mode with `svc #0`.
///
/// The EABI puts the number in `r7`, the arguments in `r0` to `r5`, and the
/// result back in `r0`. The saved `pc` already points past the `svc`, so the
/// program resumes at the next instruction. `exit` and `exit_group` leave USR
/// mode here, before `dispatch`, exactly as on the other two architectures.
///
/// # Errors
///
/// A system call from SVC mode, which is a kernel bug, or an `execve` this path
/// does not yet honour.
pub(crate) fn system_call(frame: &mut TrapFrame) -> Result<(), &'static str> {
    use crate::syscall::{Outcome, SyscallArgs, dispatch};
    use ferrix_linux_abi::nr::Syscall;

    if !frame.came_from_user() {
        return Err("a system call from SVC mode");
    }
    let [r0, r1, r2, r3, r4, r5, _, r7, ..] = frame.r;
    let args = SyscallArgs {
        number: r7 as usize,
        args: [
            r0.into(),
            r1.into(),
            r2.into(),
            r3.into(),
            r4.into(),
            r5.into(),
        ],
    };

    match super::decode_syscall(args.number) {
        Some(Syscall::Exit | Syscall::ExitGroup) => {
            // SAFETY: `came_from_user` above, and only `run_user` enters USR
            // mode, so the parked stack pointer and registers are where it
            // left them.
            unsafe { leave_user(args.args[0] as i32) }
        }
        // `set_tls` writes a coprocessor register, which is a fact about this
        // processor rather than about the process, so it is answered here for
        // the same reason x86-64 answers `arch_prctl` in its own trap path.
        // It cannot fail: any value is a valid thread pointer to hold, and
        // Linux returns zero without looking at it.
        Some(Syscall::ArmSetTls) => {
            cpu::write_tpidruro(r0);
            if let Some(result) = frame.r.first_mut() {
                *result = 0;
            }
            return Ok(());
        }
        _ => {}
    }

    match dispatch(&args) {
        Outcome::Return(value) => {
            if let Some(result) = frame.r.first_mut() {
                *result = value as u32;
            }
            Ok(())
        }
        Outcome::Enter { .. } => Err("execve through the USR trap path is not wired yet"),
    }
}

/// Leave USR mode, returning `status` from [`run_user`].
///
/// # Safety
///
/// Must be called from inside a system call made by a program [`run_user`]
/// started, on the same processor.
unsafe fn leave_user(status: i32) -> ! {
    // SAFETY: the caller guarantees the parked stack pointer and callee-saved
    // registers are still where `ferrix_run_user` put them.
    unsafe { ferrix_leave_user(status) }
}

unsafe extern "C" {
    /// The vector table, aligned as `VBAR` requires.
    static ferrix_vectors: [u8; 8 * 4];
}

/// Where the save sequence hands over.
///
/// Not called from Rust — the `bl` in the assembly above is its only caller.
#[unsafe(no_mangle)]
extern "C" fn ferrix_trap_entry(frame: &mut TrapFrame) {
    crate::trap::dispatch(frame);
}

/// Fault status: the long-descriptor format's status field, bits 5:0. The
/// format is the long one because `TTBCR.EAE` is set, and it is AArch64's.
const FSR_STATUS: u32 = 0b11_1111;
/// Data fault status: the access was a write.
const DFSR_WRITE: u32 = 1 << 11;

/// Status with the level bits masked off: what kind of fault, not where.
const STATUS_KIND: u32 = 0b11_1100;
/// A translation fault: nothing mapped.
const STATUS_TRANSLATION: u32 = 0b00_0100;
/// An access-flag fault: mapped, never touched. Treated as nothing mapped.
const STATUS_ACCESS_FLAG: u32 = 0b00_1000;
/// A permission fault: mapped, and the mapping refused the access.
const STATUS_PERMISSION: u32 = 0b00_1100;
/// A debug event, which is how a `bkpt` arrives: as a prefetch abort.
const STATUS_DEBUG: u32 = 0b10_0010;
/// An alignment fault.
const STATUS_ALIGNMENT: u32 = 0b10_0001;

/// Turn an ARMv7-A trap frame into the architecture-neutral description the
/// generic dispatcher works with.
pub(crate) fn classify(frame: &TrapFrame) -> crate::trap::Trap {
    use crate::trap::Trap;

    match frame.kind {
        // As on AArch64, the number of the interrupt is the controller's to
        // say, not the exception's; what is reported is the vector entry.
        KIND_IRQ => Trap::Interrupt(frame.kind),
        KIND_UNDEFINED => Trap::IllegalInstruction,
        KIND_SVC => Trap::SystemCall,
        KIND_PREFETCH_ABORT | KIND_DATA_ABORT => abort(frame),
        _ => Trap::Fault {
            name: kind_name(frame.kind),
            code: u64::from(frame.kind),
        },
    }
}

/// Decode a prefetch or data abort.
fn abort(frame: &TrapFrame) -> crate::trap::Trap {
    use crate::trap::{PageFault, Trap};

    let data = frame.kind == KIND_DATA_ABORT;
    let status = frame.fsr & FSR_STATUS;
    if !data && status == STATUS_DEBUG {
        return Trap::Breakpoint;
    }
    if status == STATUS_ALIGNMENT {
        return Trap::Fault {
            name: "alignment fault",
            code: u64::from(frame.fsr),
        };
    }

    match status & STATUS_KIND {
        STATUS_TRANSLATION | STATUS_ACCESS_FLAG | STATUS_PERMISSION => Trap::PageFault(PageFault {
            address: u64::from(frame.far),
            // The write bit means anything only for a data abort.
            write: data && frame.fsr & DFSR_WRITE != 0,
            execute: !data,
            user: frame.came_from_user(),
            present: status & STATUS_KIND == STATUS_PERMISSION,
        }),
        _ => Trap::Fault {
            name: if data { "data abort" } else { "prefetch abort" },
            code: u64::from(frame.fsr),
        },
    }
}

/// A readable name for a vector entry.
const fn kind_name(kind: u32) -> &'static str {
    match kind {
        KIND_RESET => "reset",
        KIND_UNDEFINED => "undefined instruction",
        KIND_SVC => "supervisor call",
        KIND_PREFETCH_ABORT => "prefetch abort",
        KIND_DATA_ABORT => "data abort",
        KIND_HYP_TRAP => "hypervisor trap",
        KIND_IRQ => "IRQ",
        KIND_FIQ => "FIQ",
        _ => "exception",
    }
}

/// Print the interrupted state.
pub(crate) fn report_trap(frame: &TrapFrame) {
    use crate::console::println;

    println!(
        "  vector   {} ({})  fsr {:#010x}  far {:#010x}",
        frame.kind,
        kind_name(frame.kind),
        frame.fsr,
        frame.far
    );
    println!(
        "  pc       {:#010x}  cpsr {:#010x}  lr {:#010x}",
        frame.pc, frame.cpsr, frame.lr
    );
    for quad in 0..4 {
        let register = |index: usize| frame.r.get(index).copied().unwrap_or(0);
        let first = quad * 4;
        println!(
            "  r{:<2} {:#010x}  r{:<2} {:#010x}  r{:<2} {:#010x}  r{:<2} {:#010x}",
            first,
            register(first),
            first + 1,
            register(first + 1),
            first + 2,
            register(first + 2),
            first + 3,
            register(first + 3),
        );
    }
    println!(
        "  from     {}",
        if frame.came_from_user() {
            "user mode"
        } else {
            "the kernel"
        }
    );
}

/// Install the vector table on this core.
///
/// # Safety
///
/// Must be called once per CPU, before anything on it can fault and before it
/// unmasks interrupts. `VBAR` and `SCTLR` are banked per core, and the one
/// table serves every core: it holds code, and no per-core state.
pub(crate) unsafe fn init() {
    // The kernel is linked below 4 GiB, so the address fits the register.
    let table = (&raw const ferrix_vectors).addr() as u32;
    // SAFETY: `table` is the vector table in this image, 32-byte aligned as
    // `VBAR` requires, and every entry branches to a real stub.
    unsafe { cpu::install_vectors(table) };
    // Here because this runs on every core, and both registers are per core.
    cpu::enable_user_fpu();
}

/// Raise a breakpoint, so the boot self-check can prove the trap path runs.
pub(crate) fn breakpoint() {
    // SAFETY: `bkpt` raises a prefetch abort the vector table handles. Like
    // AArch64's `brk`, the return address it leaves is the instruction
    // itself, which is why returning needs `advance_past_breakpoint`.
    unsafe {
        core::arch::asm!("bkpt #0", options(nomem, nostack));
    }
}

/// Step the return address over a breakpoint: every ARM instruction is four
/// bytes, and this kernel is ARM code throughout.
pub(crate) const fn advance_past_breakpoint(frame: &mut TrapFrame) {
    frame.pc += 4;
}

/// Whether `frame` was interrupted with IRQs masked, for the dispatcher's
/// benefit when it reports a fault.
#[expect(
    dead_code,
    reason = "AUDIT: named so the IRQ mask bit has one definition; used from stage 4"
)]
pub(crate) const fn irqs_were_masked(frame: &TrapFrame) -> bool {
    frame.cpsr & cpu::CPSR_IRQ_MASKED != 0
}
