//! Signal frames on x86-64, and the way back to ring 3 that restores every
//! register.
//!
//! # The frame
//!
//! Linux's `struct rt_sigframe` from `arch/x86/include/asm/sigframe.h`, the
//! same layout QEMU's `linux-user/i386/signal.c` writes:
//!
//! | Offset | Field |
//! |---|---|
//! | 0 | `pretcode`: the restorer, which the handler's `ret` pops |
//! | 8 | `struct ucontext`: flags, link, `uc_stack`, `uc_mcontext` (256 bytes), `uc_sigmask` |
//! | 312 | `struct siginfo`, 128 bytes |
//!
//! and below it, 64-byte aligned, the 512-byte `FXSAVE` area `uc_mcontext.fpstate`
//! points at. There is no vDSO, and x86-64 has no other way back from a
//! handler, so a handler installed without `SA_RESTORER` cannot be entered:
//! Linux refuses the frame, and so does this. Every libc sets it.
//!
//! # Why `rt_sigreturn` does not leave through `SYSRET`
//!
//! `SYSRET` loads the instruction pointer from `RCX` and the flags from `R11`,
//! so a program resumed through it gets those two registers back as the
//! address and the flags rather than as what the interrupted code held in
//! them. A handler can interrupt code anywhere, with `RCX` live, so the return
//! from one leaves through the trap stub's `IRETQ` path instead, which restores
//! all sixteen. Linux does the same.

use ferrix_bootinfo::is_user_address;
use ferrix_linux_abi::types::SA_RESTORER;

use super::gdt;
use super::switch::{self, UserState};
use super::syscall::SyscallFrame;
use super::trap::TrapFrame;
use crate::signal_frame::{BadFrame, FrameBytes, FrameRequest, Restored, SIGINFO_BYTES};
use crate::user::space::AddressSpace;

/// Bytes below the stack pointer the System V ABI lets a leaf function use
/// without moving it, which a frame must not overwrite.
pub(crate) const SIGNAL_RED_ZONE: u64 = 128;

/// Where `struct ucontext` starts in the frame.
const UC: usize = 8;
/// Where `uc_stack` is.
const UC_STACK: usize = UC + 16;
/// Where `uc_mcontext`, `struct sigcontext`, is.
const MCONTEXT: usize = UC + 40;
/// Where `uc_sigmask` is.
const UC_SIGMASK: usize = UC + 296;
/// Where the `siginfo` is.
const INFO: usize = UC + 304;
/// Bytes in the frame.
const FRAME_BYTES: usize = INFO + SIGINFO_BYTES;

/// `sigcontext.eflags`, relative to `uc_mcontext`. The seventeen general
/// registers come before it, in [`UserContext::registers`]'s order.
const EFLAGS: usize = 136;
/// `sigcontext.cs`, a 16-bit selector.
const CS: usize = 144;
/// `sigcontext.ss`, a 16-bit selector where Linux used to have padding.
const SS: usize = 150;
/// `sigcontext.oldmask`: the saved mask's first word, for old readers.
const OLDMASK: usize = 168;
/// `sigcontext.fpstate`: where the `FXSAVE` area is, or zero for none.
const FPSTATE: usize = 184;

/// `UC_SIGCONTEXT_SS | UC_STRICT_RESTORE_SS`: the frame records `SS`, and a
/// return restores it. What Linux writes on a machine without `XSAVE` in use.
const UC_FLAGS: u64 = 0x2 | 0x4;

/// Bytes in the `FXSAVE` area.
const FXSAVE_BYTES: usize = 512;
/// Where `MXCSR` is in the area.
const MXCSR: usize = 24;
/// Where `MXCSR_MASK` is: which `MXCSR` bits this processor has.
const MXCSR_MASK: usize = 28;
/// Where the software-reserved bytes' first magic word is. Zero says the area
/// is plain `FXSAVE`, with no extended state after it.
const SW_MAGIC1: usize = 464;
/// The mask to assume when a processor reports none: Intel's documented
/// default, every bit but `DAZ`.
const DEFAULT_MXCSR_MASK: u32 = 0xFFBF;

/// The flags a frame may give back: carry, parity, adjust, zero, sign,
/// direction, overflow and alignment check -- Linux's `FIX_EFLAGS` without the
/// trap and resume flags, since a trap flag set from a frame would single-step
/// the program into a kernel that has no `#DB` handler for it. Never `IOPL`,
/// never interrupts off.
const FLAGS_RESTORABLE: u64 =
    (1 << 0) | (1 << 2) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 10) | (1 << 11) | (1 << 18);
/// The flags every program runs with: interrupts on, and the reserved bit
/// that always reads as one.
const FLAGS_ALWAYS: u64 = 0x202;
/// The flags a handler starts with cleared: trap, direction and resume, as on
/// Linux, so a handler runs its string instructions forwards.
const FLAGS_CLEARED_FOR_HANDLER: u64 = (1 << 8) | (1 << 10) | (1 << 16);

/// A program's registers as the way back to ring 3 will load them: the whole
/// trap frame, whichever way the program came in.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub(crate) struct UserContext(TrapFrame);

impl UserContext {
    /// The registers a trap from ring 3 saved.
    pub(crate) const fn from_trap(frame: &TrapFrame) -> UserContext {
        UserContext(*frame)
    }

    /// Put them back where the trap stub restores them from.
    pub(crate) const fn store_trap(&self, frame: &mut TrapFrame) {
        *frame = self.0;
    }

    /// The program's stack pointer.
    pub(crate) const fn stack_pointer(&self) -> u64 {
        self.0.rsp
    }

    /// The value in the return register, read as a signed result: what a
    /// system call left there, which the restart logic inspects for a
    /// kernel-internal restart code.
    pub(crate) const fn syscall_result(&self) -> isize {
        self.0.rax as isize
    }

    /// Overwrite the return register with `value`: how the restart logic turns
    /// a restart code into `EINTR` for a call that will not be restarted.
    pub(crate) const fn set_syscall_result(&mut self, value: isize) {
        self.0.rax = value as u64;
    }

    /// Rewind so the interrupted `syscall` re-executes when the program
    /// resumes, as Linux's `arch_do_signal_or_restart` does. The two-byte
    /// `syscall` opcode sits at `RIP - 2`, and `RAX` carried both the number
    /// and the result, so it is restored to the number the call is re-entered
    /// with -- `restart_syscall`'s number for a `restart_block` resume, the
    /// original number otherwise. The System V argument registers were never
    /// clobbered, so `orig_arg0` is not needed here.
    pub(crate) const fn rewind_syscall(
        &mut self,
        orig_nr: u64,
        orig_arg0: u64,
        restart_block: bool,
    ) {
        let _ = orig_arg0;
        self.0.rax = if restart_block {
            ferrix_linux_abi::nr::x86_64::RESTART_SYSCALL as u64
        } else {
            orig_nr
        };
        self.0.rip = self.0.rip.wrapping_sub(2);
    }

    /// The registers a system call saved. `SYSCALL` put the return address in
    /// `RCX` and the flags in `R11`, so those two are also what the program
    /// holds in them when it resumes.
    pub(super) const fn from_syscall(frame: &SyscallFrame) -> UserContext {
        UserContext(TrapFrame {
            r15: frame.r15,
            r14: frame.r14,
            r13: frame.r13,
            r12: frame.r12,
            r11: frame.r11,
            r10: frame.r10,
            r9: frame.r9,
            r8: frame.r8,
            rbp: frame.rbp,
            rdi: frame.rdi,
            rsi: frame.rsi,
            rdx: frame.rdx,
            rcx: frame.rcx,
            rbx: frame.rbx,
            rax: frame.rax,
            vector: 0,
            error_code: 0,
            rip: frame.rcx,
            cs: USER_CS,
            rflags: frame.r11,
            rsp: frame.user_rsp,
            ss: USER_SS,
        })
    }

    /// Put them back where the `SYSRET` path restores them from. That path
    /// takes the address from `RCX` and the flags from `R11`, so those are
    /// what goes there: right for entering a handler, whose `RCX` and `R11`
    /// hold nothing, and never used to resume an interrupted context.
    pub(super) const fn store_syscall(&self, frame: &mut SyscallFrame) {
        let regs = &self.0;
        frame.r15 = regs.r15;
        frame.r14 = regs.r14;
        frame.r13 = regs.r13;
        frame.r12 = regs.r12;
        frame.rbp = regs.rbp;
        frame.rbx = regs.rbx;
        frame.r9 = regs.r9;
        frame.r8 = regs.r8;
        frame.r10 = regs.r10;
        frame.rdx = regs.rdx;
        frame.rsi = regs.rsi;
        frame.rdi = regs.rdi;
        frame.rax = regs.rax;
        frame.r11 = regs.rflags;
        frame.rcx = regs.rip;
        frame.user_rsp = regs.rsp;
    }

    /// The seventeen registers `struct sigcontext` opens with, in its order.
    const fn registers(&self) -> [u64; 17] {
        let r = &self.0;
        [
            r.r8, r.r9, r.r10, r.r11, r.r12, r.r13, r.r14, r.r15, r.rdi, r.rsi, r.rbp, r.rbx,
            r.rdx, r.rax, r.rcx, r.rsp, r.rip,
        ]
    }

    /// Load the seventeen from a `struct sigcontext`'s order.
    const fn set_registers(&mut self, values: [u64; 17]) {
        let [
            r8,
            r9,
            r10,
            r11,
            r12,
            r13,
            r14,
            r15,
            rdi,
            rsi,
            rbp,
            rbx,
            rdx,
            rax,
            rcx,
            rsp,
            rip,
        ] = values;
        let r = &mut self.0;
        r.r8 = r8;
        r.r9 = r9;
        r.r10 = r10;
        r.r11 = r11;
        r.r12 = r12;
        r.r13 = r13;
        r.r14 = r14;
        r.r15 = r15;
        r.rdi = rdi;
        r.rsi = rsi;
        r.rbp = rbp;
        r.rbx = rbx;
        r.rdx = rdx;
        r.rax = rax;
        r.rcx = rcx;
        r.rsp = rsp;
        r.rip = rip;
    }
}

/// Ring 3's code selector, with its requested privilege level.
const USER_CS: u64 = (gdt::USER_CODE | 3) as u64;
/// Ring 3's stack selector, likewise.
const USER_SS: u64 = (gdt::USER_DATA | 3) as u64;

/// Write `request`'s frame below its stack and point `context` at the handler:
/// `RDI` the signal, `RSI` the `siginfo`, `RDX` the `ucontext`, and the stack
/// pointer at `pretcode`, as if the handler had just been called from it.
pub(crate) fn setup_signal_frame(
    space: &AddressSpace,
    context: &mut UserContext,
    request: &FrameRequest,
) -> Result<(), BadFrame> {
    if request.flags & SA_RESTORER == 0 {
        return Err(BadFrame);
    }
    let fpstate = request
        .stack
        .checked_sub(FXSAVE_BYTES as u64)
        .ok_or(BadFrame)?
        & !63;
    // Aligned so that on entry `(sp + 8) % 16 == 0`, which is what a function
    // expects just after a `call` pushed its return address.
    let frame_at = ((fpstate.checked_sub(FRAME_BYTES as u64).ok_or(BadFrame)? + 8) & !15)
        .checked_sub(8)
        .ok_or(BadFrame)?;

    // SAFETY: on the running task's own way back to ring 3, so the processor
    // holds this program's x87 and SSE registers.
    let state = unsafe { UserState::capture() };
    let mut area = FrameBytes::zeroed(FXSAVE_BYTES);
    area.put(0, state.fxsave())?;
    area.put_u32(SW_MAGIC1, 0)?;
    area.write(space, fpstate)?;

    let mut frame = FrameBytes::zeroed(FRAME_BYTES);
    frame.put_u64(0, request.restorer)?;
    frame.put_u64(UC, UC_FLAGS)?;
    frame.put_stack(UC_STACK, request.altstack)?;
    for (index, value) in context.registers().iter().enumerate() {
        frame.put_u64(MCONTEXT + index * 8, *value)?;
    }
    frame.put_u64(MCONTEXT + EFLAGS, context.0.rflags)?;
    frame.put(MCONTEXT + CS, &(gdt::USER_CODE | 3).to_le_bytes())?;
    frame.put(MCONTEXT + SS, &(gdt::USER_DATA | 3).to_le_bytes())?;
    frame.put_u64(MCONTEXT + OLDMASK, request.mask)?;
    frame.put_u64(MCONTEXT + FPSTATE, fpstate)?;
    frame.put_u64(UC_SIGMASK, request.mask)?;
    frame.put(INFO, &request.info)?;
    frame.write(space, frame_at)?;

    let regs = &mut context.0;
    regs.rdi = u64::from(request.signal);
    regs.rsi = frame_at + INFO as u64;
    regs.rdx = frame_at + UC as u64;
    regs.rax = 0;
    regs.rsp = frame_at;
    regs.rip = request.handler;
    regs.rflags &= !FLAGS_CLEARED_FOR_HANDLER;
    regs.cs = USER_CS;
    regs.ss = USER_SS;
    Ok(())
}

/// Read the frame a handler returned through -- its restorer's `ret` left the
/// stack pointer just past `pretcode` -- and load `context` from it.
///
/// Refused: an instruction pointer or stack pointer outside the user half,
/// which `IRETQ` would fault on in ring 0. The flags are masked to what a
/// program may set, the selectors are ring 3's whatever the frame says, and
/// `MXCSR` is masked to the bits the processor has before `FXRSTOR64` sees it.
pub(crate) fn restore_signal_frame(
    space: &AddressSpace,
    context: &mut UserContext,
    rt: bool,
) -> Result<Restored, BadFrame> {
    let _ = rt;
    let frame_at = context.0.rsp.checked_sub(8).ok_or(BadFrame)?;
    let frame = FrameBytes::read(space, frame_at, INFO)?;
    let mut registers = [0_u64; 17];
    for (index, slot) in registers.iter_mut().enumerate() {
        *slot = frame.u64_at(MCONTEXT + index * 8)?;
    }
    let [.., rsp, rip] = registers;
    if !is_user_address(rip) || !is_user_address(rsp) {
        return Err(BadFrame);
    }
    let flags = frame.u64_at(MCONTEXT + EFLAGS)?;
    let fpstate = frame.u64_at(MCONTEXT + FPSTATE)?;
    let restored = Restored {
        mask: frame.u64_at(UC_SIGMASK)?,
        altstack: frame.stack_at(UC_STACK)?,
    };
    if fpstate != 0 {
        restore_fpu(space, fpstate)?;
    }
    context.set_registers(registers);
    context.0.rflags = flags & FLAGS_RESTORABLE | FLAGS_ALWAYS;
    context.0.cs = USER_CS;
    context.0.ss = USER_SS;
    Ok(restored)
}

/// Load the x87 and SSE registers from the `FXSAVE` area at `at`.
fn restore_fpu(space: &AddressSpace, at: u64) -> Result<(), BadFrame> {
    let area = FrameBytes::read(space, at, FXSAVE_BYTES)?;
    // SAFETY: inside the running task's own system call, so the registers are
    // its own; captured only to learn `MXCSR_MASK` and to carry the area.
    let mut state = unsafe { UserState::capture() };
    let mut live = FrameBytes::zeroed(FXSAVE_BYTES);
    live.put(0, state.fxsave())?;
    let mask = match live.u32_at(MXCSR_MASK)? {
        0 => DEFAULT_MXCSR_MASK,
        mask => mask,
    };
    let mut wanted = FrameBytes::zeroed(FXSAVE_BYTES);
    wanted.put(0, area.get(0, FXSAVE_BYTES)?)?;
    wanted.put_u32(MXCSR, area.u32_at(MXCSR)? & mask)?;
    state
        .fxsave_mut()
        .copy_from_slice(wanted.get(0, FXSAVE_BYTES)?);
    // SAFETY: the registers are the running task's, and `MXCSR` was masked to
    // the bits this processor reports, so `FXRSTOR64` has nothing to refuse.
    unsafe { switch::load_fpu(&state) };
    Ok(())
}

unsafe extern "C" {
    /// Load `frame` onto the stack pointer and leave through the trap stub's
    /// restore path: every register, and `IRETQ`.
    fn ferrix_resume_trap_frame(frame: *const TrapFrame) -> !;
}

/// Resume ring 3 from `context` through `IRETQ`, restoring every register.
/// Does not return.
///
/// # Safety
///
/// Must be called by a user task with its address space and user state loaded,
/// from its own kernel stack, with nothing owned left on the stack above, and
/// with `context`'s selectors ring 3's and its instruction and stack pointers
/// user addresses -- which [`restore_signal_frame`] and
/// [`setup_signal_frame`] leave true.
pub(super) unsafe fn resume_context(context: &UserContext) -> ! {
    // SAFETY: the caller's guarantee is the assembly's contract.
    unsafe { ferrix_resume_trap_frame(core::ptr::from_ref(&context.0)) }
}
