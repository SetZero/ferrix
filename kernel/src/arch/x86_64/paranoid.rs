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
//!   path, whose `CS` test is right for a ring-3 frame. The other three stay:
//!   an NMI from ring 3 is counted and returned from, and the other two stop.
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
//!   stacks: nothing may arm one there. The boot check below is the only code
//!   that arms breakpoints; an interface that lets anything else must refuse
//!   `ferrix_paranoid_stubs` and the stacks, as Linux refuses `noinstr` text.
//!   This is Linux's scheme since 5.10, which retired its IST shift for it.
//! * **An NMI cannot nest.** The processor delivers no NMI after one until the
//!   next `iretq`, and the handler's own is its last instruction. An exception
//!   inside the handler that returned would be an earlier `iretq`, so the
//!   handler takes none: it touches statics and per-CPU records mapped since
//!   boot, never the demand window, prints nothing, and `DR7` is clear. Linux's
//!   nested-NMI frame juggling exists because its NMI handlers do fault.
//! * **`#MC` and `#DF` never return**, and a second on the same stack is the
//!   report failing.

use core::hint::spin_loop;
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_sched::{CpuSet, NICE_0_WEIGHT};
use ferrix_sync::IrqControl;

use super::cpu;
use super::trap::TrapFrame;
use crate::console::println;
use crate::panic::catalog;
use crate::sync::SpinLock;

/// `#DB`.
const DEBUG: u64 = 1;
/// The NMI.
const NMI: u64 = 2;
/// `#MC`.
const MACHINE_CHECK: u64 = 18;

/// `DR6` with no condition recorded: the fixed-one bits set, the rest clear.
/// Linux's `DR6_RESERVED`.
const DR6_CLEAR: u64 = 0xFFFE_0FF0;
/// `DR6`'s B0 to B3: which hardware breakpoint matched.
const DR6_BREAKPOINTS: u64 = 0xF;
/// `RFLAGS.RF`: the instruction returned to runs without matching an
/// instruction breakpoint.
const RFLAGS_RF: u64 = 1 << 16;

/// NMIs taken.
static NMIS: AtomicU64 = AtomicU64::new(0);
/// Kernel `#DB`s taken for each hardware breakpoint, by the slot DR6 named.
static BREAKPOINTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// Paranoid handlers that found `GS` naming no processor's record.
static FOREIGN_GS: AtomicU64 = AtomicU64::new(0);
/// Runs of [`debug_hook`].
static HOOK_RUNS: AtomicU64 = AtomicU64::new(0);

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
        DEBUG => debug(frame),
        NMI => {
            note_gs();
            let _ = NMIS.fetch_add(1, Ordering::Relaxed);
        }
        MACHINE_CHECK => crate::trap::fatal(frame, "machine check", &catalog::MACHINE_CHECK),
        vector => crate::trap::fatal(
            frame,
            super::trap::vector_name(vector),
            &catalog::UNEXPECTED_EXCEPTION,
        ),
    }
}

/// A `#DB` taken in the kernel: a hardware breakpoint is counted and returned
/// from; anything else -- a single step, a general detect -- is nothing the
/// kernel asked for, and stops it.
fn debug(frame: &mut TrapFrame) {
    let status = take_debug_status();
    if status & DR6_BREAKPOINTS == 0 {
        crate::trap::fatal(
            frame,
            "a debug exception in the kernel that no hardware breakpoint raised",
            &catalog::UNEXPECTED_EXCEPTION,
        );
    }
    for (slot, count) in BREAKPOINTS.iter().enumerate() {
        if status & (1 << slot) != 0 {
            let _ = count.fetch_add(1, Ordering::Relaxed);
        }
    }
    note_gs();

    // An instruction breakpoint is a fault: the saved RIP is the instruction
    // that matched, and returning to it without RF matches it again, for ever.
    // RF lets exactly that instruction run. A data breakpoint is a trap, which
    // needs none, and RF costs it nothing.
    frame.rflags |= RFLAGS_RF;
    debug_hook();
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

/// Count a handler that finds `GS` naming no processor's record -- which, once
/// the records exist, only a wrong `swapgs` decision can do.
///
/// Compares the register against every record rather than following it: that
/// is what `this_cpu_for_report` is for, and following a program's `GS` from
/// here would fault inside a handler that must not.
fn note_gs() {
    if crate::smp::topology().is_some() && crate::smp::this_cpu_for_report().is_err() {
        let _ = FOREIGN_GS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Called by every kernel `#DB` handler run, so that the nesting check can put
/// a breakpoint on code the handler executes.
#[inline(never)]
fn debug_hook() {
    let _ = HOOK_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// The instruction the nesting check breaks on first.
#[inline(never)]
fn breakpoint_target() -> u64 {
    core::hint::black_box(HOOK_RUNS.load(Ordering::Relaxed))
}

/// `DR7`'s local enable for `slot`. The slot's type and length fields left
/// zero mean an instruction breakpoint, one byte.
const fn dr7_local(slot: usize) -> u64 {
    1 << (slot * 2)
}

/// Kernel `#DB`s taken so far for breakpoint `slot`.
fn hits(slot: usize) -> u64 {
    BREAKPOINTS
        .get(slot)
        .map_or(0, |count| count.load(Ordering::Relaxed))
}

unsafe extern "C" {
    /// `LSTAR`'s target, whose first instruction is `swapgs` on the program's
    /// stack.
    static ferrix_syscall_stub: [u8; 0];
    /// The trampoline's `sysretq`, after the `swapgs` back, still on the
    /// program's stack.
    static ferrix_syscall_sysret: [u8; 0];
}

/// How long the system call window check may take, program and all.
const WINDOW_PATIENCE_NANOS: u64 = 30_000_000_000;

/// How long an NMI sent to this processor has to arrive.
const NMI_PATIENCE_NANOS: u64 = 1_000_000_000;

/// What the pinned task of [`check_system_call_window`] found: the program's
/// status, or why it has none. `None` while it runs.
static WINDOW: SpinLock<Option<Result<i32, &'static str>>> = SpinLock::new(None);

/// The boot checks for these entries, each of which halts the machine or
/// fails if the entry is not there.
///
/// # Errors
///
/// What failed, for the stage 3 report.
pub(crate) fn check() -> Result<(), &'static str> {
    let nmis = check_nmi()?;
    let (entries, returns, status) = check_system_call_window()?;
    let hook_runs = check_breakpoints_do_not_nest()?;
    println!(
        "  nmi      {nmis} NMI sent to this processor with interrupts masked was taken on its \
         own stack and returned from"
    );
    println!(
        "  debug    {entries} breakpoints on the system call entry and {returns} on its \
         sysretq, each on the program's stack, found the kernel's GS; the program exited \
         with {status}"
    );
    println!(
        "  debug    a breakpoint on code the #DB handler runs did not fire inside it: \
         {hook_runs} handler runs, one stack"
    );
    Ok(())
}

/// An NMI taken in the kernel is survived.
///
/// Sent to this processor through its own local APIC with interrupts masked,
/// which an NMI ignores. Without the paranoid entry vector 2 is fatal.
fn check_nmi() -> Result<u64, &'static str> {
    let before = NMIS.load(Ordering::Relaxed);
    let foreign = FOREIGN_GS.load(Ordering::Relaxed);

    let saved = <super::Irq as IrqControl>::disable();
    let sent = super::apic::send_nmi_to_self();
    let deadline = crate::timer::now_nanos().saturating_add(NMI_PATIENCE_NANOS);
    while sent.is_ok()
        && NMIS.load(Ordering::Relaxed) == before
        && crate::timer::now_nanos() < deadline
    {
        spin_loop();
    }
    <super::Irq as IrqControl>::restore(saved);

    sent?;
    let taken = NMIS.load(Ordering::Relaxed).wrapping_sub(before);
    if taken == 0 {
        return Err("an NMI sent to this processor never arrived");
    }
    if FOREIGN_GS.load(Ordering::Relaxed) != foreign {
        return Err("an NMI handler ran with a GS that names no processor's record");
    }
    Ok(taken)
}

/// A `#DB` in the `SYSCALL` trampoline's ring-0 stretches on the program's
/// stack is survived, with the kernel's `GS`.
///
/// Instruction breakpoints on the stub's first instruction -- `swapgs`, with
/// the program's `GS` and stack -- and on its `sysretq`, after the `swapgs`
/// back, while [`super::USER_TEST_PROGRAM`] writes a line and exits. Debug
/// registers are per processor, so the program and the task that arms them
/// are pinned to one. Returns the hits at each, and the program's status.
fn check_system_call_window() -> Result<(u64, u64, i32), &'static str> {
    let cpu = crate::smp::this_cpu()
        .ok_or("the per-CPU register is not installed")?
        .logical;
    let entries = hits(0);
    let returns = hits(1);
    let foreign = FOREIGN_GS.load(Ordering::Relaxed);
    *WINDOW.lock() = None;

    let task = crate::sched::spawn_on(
        "syscall window",
        window_task,
        cpu,
        NICE_0_WEIGHT,
        cpu,
        CpuSet::of(cpu),
    )?;
    let deadline = crate::timer::now_nanos().saturating_add(WINDOW_PATIENCE_NANOS);
    let outcome = loop {
        if let Some(outcome) = *WINDOW.lock() {
            break outcome;
        }
        if crate::timer::now_nanos() > deadline {
            return Err("the system call window check never finished");
        }
        crate::sched::sleep_for(1_000_000);
    };
    while !task.is_dead() && crate::timer::now_nanos() < deadline {
        crate::sched::sleep_for(1_000_000);
    }
    drop(task);
    let _ = crate::sched::reap();

    let status = outcome?;
    let entries = hits(0).wrapping_sub(entries);
    let returns = hits(1).wrapping_sub(returns);
    if FOREIGN_GS.load(Ordering::Relaxed) != foreign {
        return Err("a #DB in the system call window ran with the program's GS");
    }
    // Two calls, `write` and `exit_group`, and only the first returns.
    if entries < 2 || returns < 1 {
        return Err("a breakpoint in the system call window did not fire");
    }
    if status != super::USER_TEST_STATUS {
        return Err("the program with breakpoints in its system calls exited wrongly");
    }
    Ok((entries, returns, status))
}

/// The pinned half of [`check_system_call_window`], on processor `cpu`.
fn window_task(cpu: usize) {
    let outcome = run_with_window_breakpoints(cpu);
    *WINDOW.lock() = Some(outcome);
}

/// Arm the two breakpoints on this processor, run the program on it, disarm.
fn run_with_window_breakpoints(cpu: usize) -> Result<i32, &'static str> {
    use crate::syscall::exec::{Executable, load_executable};

    let file = crate::syscall::image::build_with(
        ferrix_elf::Class::Elf64,
        super::ARCH.elf_machine(),
        crate::syscall::image::Shape::Good,
        super::USER_TEST_PROGRAM,
    );
    let program = Executable {
        image: &file,
        exe: b"/window",
        exec_fn: b"/window",
    };
    let process = load_executable(
        program,
        &[b"/window"],
        &[],
        [0x5a; ferrix_ustack::RANDOM_BYTES],
    )
    .map_err(|_| "the system call window program could not be loaded")?;

    let entry = (&raw const ferrix_syscall_stub) as u64;
    let sysret = (&raw const ferrix_syscall_sysret) as u64;
    // SAFETY: an instruction of the trampoline, not in the paranoid entry nor
    // on an IST stack, which is what a `#DB` there needs; unarmed until DR7.
    unsafe { cpu::write_breakpoint_address(0, entry) };
    // SAFETY: as above, for the trampoline's `sysretq`.
    unsafe { cpu::write_breakpoint_address(1, sysret) };
    // SAFETY: arms the two slots just written, and nothing else.
    unsafe { cpu::write_dr7(dr7_local(0) | dr7_local(1)) };

    let status = crate::syscall::process::start_on(&process, Some(cpu))
        .ok()
        .and_then(|_task| process.wait_for_exit(u64::MAX));

    // SAFETY: disarming changes nothing but that they stop firing. This task
    // is pinned, so this is the processor they were armed on.
    unsafe { cpu::write_dr7(0) };

    // The GS base the program ran with, parked on the kernel's side. If it were
    // a per-CPU record, both halves of `swapgs` would hold the same address,
    // and a handler that skipped the swap would find the kernel's GS anyway:
    // the breakpoints above would prove nothing.
    // SAFETY: a kernel task, with the kernel's GS, outside the trampoline.
    let program_gs = unsafe { super::syscall::program_gs_base() };
    if is_cpu_record(program_gs) {
        return Err("a program ran with a per-CPU record as its GS base");
    }
    status.ok_or("the system call window program could not be started")
}

/// Whether `address` is one of the processors' per-CPU records.
fn is_cpu_record(address: u64) -> bool {
    crate::smp::topology().is_some_and(|topology| {
        topology
            .cpus()
            .iter()
            .any(|cpu| core::ptr::from_ref(cpu) as u64 == address)
    })
}

/// A breakpoint on code the `#DB` handler itself runs does not fire inside it.
///
/// Breakpoints on [`breakpoint_target`] and on [`debug_hook`], which every
/// kernel `#DB` handler calls. The target's `#DB` runs the hook with no
/// breakpoint firing, because the entry cleared `DR7`; calling the hook here
/// shows its breakpoint was armed. Without the clear, the hook's `#DB` would
/// land on the top of the stack the target's handler is still using, which the
/// occupancy count reports as FX-9006. Returns the hook's runs: three, two in
/// handlers and one here.
fn check_breakpoints_do_not_nest() -> Result<u64, &'static str> {
    let targets = hits(0);
    let hooks = hits(2);
    let runs = HOOK_RUNS.load(Ordering::Relaxed);

    // Masked, so that arming, both breakpoints and disarming happen on one
    // processor.
    let saved = <super::Irq as IrqControl>::disable();
    // SAFETY: a kernel function outside the paranoid entry and the IST stacks;
    // unarmed until DR7, and disarmed below before interrupts open again.
    unsafe { cpu::write_breakpoint_address(0, breakpoint_target as fn() -> u64 as usize as u64) };
    // SAFETY: as above, for the hook every kernel `#DB` handler runs.
    unsafe { cpu::write_breakpoint_address(2, debug_hook as fn() as usize as u64) };
    // SAFETY: arms the two slots just written, and nothing else.
    unsafe { cpu::write_dr7(dr7_local(0) | dr7_local(2)) };
    let _ = core::hint::black_box(breakpoint_target());
    debug_hook();
    // SAFETY: disarming.
    unsafe { cpu::write_dr7(0) };
    <super::Irq as IrqControl>::restore(saved);

    if hits(0).wrapping_sub(targets) != 1 {
        return Err("an instruction breakpoint in the kernel did not fire exactly once");
    }
    if hits(2).wrapping_sub(hooks) != 1 {
        return Err("a breakpoint on the #DB handler's path fired other than once");
    }
    let runs = HOOK_RUNS.load(Ordering::Relaxed).wrapping_sub(runs);
    if runs != 3 {
        return Err("the #DB handler did not run once per breakpoint");
    }
    Ok(runs)
}
