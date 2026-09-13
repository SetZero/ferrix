//! What happens when the CPU stops running the program and enters the kernel.
//!
//! The architectures disagree about almost everything here — x86-64 has 256
//! vectors and an error code that exists for ten of them; AArch64 has sixteen
//! vector-table entries and a syndrome register; ARMv7-A has eight entries, five
//! processor modes and a fault status register per kind of abort — so each
//! `arch` module classifies its own frame into the [`Trap`] below, and the
//! policy is written once.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch;
use crate::console::println;

/// Why the kernel was entered, in terms every architecture shares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Trap {
    /// A translation fault: the address was not mapped, or was mapped without
    /// the permission the access needed.
    PageFault(PageFault),
    /// A debugger breakpoint. `int3` on x86-64, `brk` on AArch64.
    Breakpoint,
    /// An instruction the CPU refused to execute.
    IllegalInstruction,
    /// A device interrupt, by controller-assigned number.
    Interrupt(u32),
    /// A deliberate entry from user mode. Stage 7 gives this a body.
    SystemCall,
    /// Anything else, which at this stage means the kernel has a bug.
    Fault {
        /// The architecture's name for it.
        name: &'static str,
        /// The architecture's own code: a vector on x86-64, a syndrome on
        /// `AArch64`.
        code: u64,
    },
}

/// The details of a translation fault.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PageFault {
    /// The address the access was to.
    pub(crate) address: u64,
    /// True if the access was a write.
    pub(crate) write: bool,
    /// True if it was an instruction fetch.
    pub(crate) execute: bool,
    /// True if the fault came from user mode.
    pub(crate) user: bool,
    /// True if a mapping existed but denied the access; false if there was no
    /// mapping at all. The difference decides whether this is a protection
    /// violation or a page that has not been faulted in yet.
    pub(crate) present: bool,
}

/// Breakpoints taken since boot.
///
/// Counted rather than merely handled, so the boot self-check can prove the
/// whole path ran — entry, save, dispatch, restore, return — rather than only
/// that it did not crash.
static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);

/// Page faults the kernel resolved by mapping a page.
static FAULTS_HANDLED: AtomicU64 = AtomicU64::new(0);

/// Interrupts that arrived while a program was running in user mode.
///
/// Counted so a check can tell a program preempted in user mode from one that
/// was only ever switched away from inside a system call: both show up as a
/// task switched to more than once, and only the first needs interrupts open
/// in user mode.
static USER_INTERRUPTS: AtomicU64 = AtomicU64::new(0);

/// How many breakpoints have been taken.
pub(crate) fn breakpoint_count() -> u64 {
    BREAKPOINTS.load(Ordering::Relaxed)
}

/// How many page faults have been resolved.
pub(crate) fn handled_fault_count() -> u64 {
    FAULTS_HANDLED.load(Ordering::Relaxed)
}

/// How many interrupts have arrived while a program was in user mode.
pub(crate) fn user_interrupt_count() -> u64 {
    USER_INTERRUPTS.load(Ordering::Relaxed)
}

/// Where every trap arrives, from any architecture's entry stub.
pub(crate) fn dispatch(frame: &mut arch::TrapFrame) {
    let trap = arch::classify(frame);
    match trap {
        // A program's own breakpoint, undefined instruction or other fault is
        // the program's problem, not the kernel's: a signal, as on Linux.
        Trap::Breakpoint | Trap::IllegalInstruction | Trap::Fault { .. }
            if frame.came_from_user() =>
        {
            user_fault(frame, &trap);
        }
        Trap::Breakpoint => {
            // The architectures disagree about where the saved instruction
            // pointer lands: x86-64's `int3` pushes the address after itself,
            // AArch64's `brk` and ARMv7-A's `bkpt` the address *of* themselves.
            // Returning without asking would loop forever on two of them.
            arch::advance_past_breakpoint(frame);
            let _ = BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
        }
        Trap::PageFault(fault) => handle_page_fault(frame, fault),
        // The controller, not the CPU, knows which interrupt arrived and how
        // it is acknowledged, and the architectures disagree about both.
        // So the architecture claims, dispatches and retires; what crosses
        // back into generic code is a number.
        Trap::Interrupt(_) => {
            if frame.came_from_user() {
                let _ = USER_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
            }
            arch::service_interrupts(frame, crate::irq::dispatch);
            // And only now, with the controller told this interrupt is done,
            // may the processor go and run something else. Switching inside
            // the handler would leave an interrupt in service for as long as
            // the next task ran, and a controller still servicing one delivers
            // nothing further.
            crate::sched::preempt_on_irq_exit(frame.came_from_user());
        }
        // On x86-64 a system call never arrives here: `SYSCALL` has an entry of
        // its own. On both Arm architectures `svc` is an exception like any
        // other and this is the only way in, so the architecture decides what
        // the registers mean.
        Trap::SystemCall => {
            if let Err(why) = arch::system_call(frame) {
                fatal(frame, why, &crate::panic::catalog::SYSTEM_CALL_TRAP);
            }
        }
        Trap::IllegalInstruction => fatal(
            frame,
            "illegal instruction",
            &crate::panic::catalog::ILLEGAL_INSTRUCTION,
        ),
        Trap::Fault { name, .. } => {
            fatal(frame, name, &crate::panic::catalog::UNEXPECTED_EXCEPTION)
        }
    }

    // On the way back to a program, which is where a program killed from
    // outside finds out and a signal is delivered: a task spinning in user
    // mode reaches here on its next tick, and one that was preempted reaches
    // here when it is resumed. See `crate::syscall::deliver`.
    if frame.came_from_user() && crate::syscall::deliver::needs_attention() {
        let mut context = arch::UserContext::from_trap(frame);
        crate::syscall::deliver::return_to_user(&mut context);
        context.store_trap(frame);
    }
}

/// A trap a program's own instruction took that nothing resolves: the signal
/// Linux would raise for it, forced on the program so that it either handles
/// it or ends with it as its status. Never a kernel panic -- a program cannot
/// be allowed to stop the machine with `hlt` -- unless the kernel entered user
/// mode with no process to blame.
fn user_fault(frame: &arch::TrapFrame, trap: &Trap) {
    user_fault_as(frame, trap, arch::fault_signal(frame, trap));
}

/// [`user_fault`] with the signal, `si_code` and address already decided, for
/// a fault the architecture cannot classify alone: a touch of a file mapping
/// past the end of its file is `SIGBUS`, which only the address space knows.
fn user_fault_as(frame: &arch::TrapFrame, trap: &Trap, (signal, code, address): (u32, i32, u64)) {
    use crate::syscall::deliver;
    use crate::syscall::signal::{Origin, Posted};

    // Open while the signal is forced. A fatal one ends the process here, which
    // wakes and interrupts its other threads and, when none of them is live,
    // closes its handles and descriptors and tells whoever watches it, none of
    // which may run with interrupts masked.
    // A trap from user mode holds no kernel lock, which is also what lets
    // `deliver::return_to_user` open them from this same dispatch; they are
    // masked again before anything else here runs.
    arch::enable_interrupts();
    let posted = deliver::force(signal, Origin::Fault { code, address });
    arch::disable_interrupts();
    match posted {
        None => fatal(
            frame,
            "a fault from user mode with no process",
            &crate::panic::catalog::UNEXPECTED_EXCEPTION,
        ),
        Some(Posted::Fatal) => {
            let pid = crate::syscall::process::current().map_or(0, |process| process.pid());
            println!("  signal   pid {pid} ended by signal {signal} at {address:#x}: {trap:?}");
        }
        Some(Posted::Discarded | Posted::Pending) => {}
    }
}

/// Resolve a translation fault, or report it and stop.
///
/// Stage 3 handles exactly one case — a kernel address inside the on-demand
/// window, which is mapped and the instruction retried. That is deliberately
/// the same shape the real handler will have: a fault is resolved by *making
/// the mapping true* and returning, never by stepping over the instruction.
/// Everything else is a bug in the kernel and is fatal.
/// Try to make a user fault true through the running program's address space.
///
/// `Ok` when the instruction may be retried. An error is a real fault: an
/// address in no region, or an access the region forbids, is `SIGSEGV`; a page
/// of a file mapping past the end of its file is `SIGBUS`. `None` in the error
/// means there was no process to ask.
fn resolve_user_fault(fault: &PageFault) -> Result<(), Option<crate::user::space::SpaceError>> {
    let Some(process) = crate::syscall::process::current() else {
        // A fault from user mode with no process is not a program's mistake,
        // it is the kernel having entered ring 3 without recording who was
        // running. Reported as fatal rather than resolved.
        return Err(None);
    };
    let access = crate::user::space::Access {
        write: fault.write,
        execute: fault.execute,
    };
    process.space().fault(fault.address, access).map_err(Some)
}

fn handle_page_fault(frame: &mut arch::TrapFrame, fault: PageFault) {
    if !fault.present
        && !fault.user
        && crate::mm::is_demand_window(fault.address)
        && crate::mm::map_demand_page(fault.address).is_ok()
    {
        let _ = FAULTS_HANDLED.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // A fault in user mode is the ordinary case, not an error: every page a
    // program touches arrives this way, and so does every copy-on-write copy.
    // Resolved by making the mapping true and returning, which retries the
    // instruction -- never by stepping over it.
    //
    // `present` is deliberately not consulted. A write to a present but
    // read-only copy-on-write page is exactly the fault that must copy, and
    // filtering on `!present` here would send that instruction back to fault
    // for ever.
    let resolved = if fault.user {
        resolve_user_fault(&fault)
    } else {
        Err(None)
    };
    if resolved.is_ok() {
        let _ = FAULTS_HANDLED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if fault.user && frame.came_from_user() {
        /// `SIGBUS`'s `si_code` for an address with nothing behind it, which
        /// is what Linux reports for a touch of a file mapping past its file.
        const BUS_ADRERR: i32 = 2;
        let trap = Trap::PageFault(fault);
        match resolved {
            Err(Some(crate::user::space::SpaceError::PastEnd(address))) => user_fault_as(
                frame,
                &trap,
                (ferrix_linux_abi::types::SIGBUS, BUS_ADRERR, address),
            ),
            _ => user_fault(frame, &trap),
        }
        return;
    }

    report(
        frame,
        format_args!(
            "page fault at {:#x} ({}{}{}{})",
            fault.address,
            if fault.user { "user " } else { "kernel " },
            if fault.write { "write" } else { "read" },
            if fault.execute { ", fetch" } else { "" },
            if fault.present {
                ", protection"
            } else {
                ", not mapped"
            },
        ),
        &crate::panic::catalog::UNHANDLED_PAGE_FAULT,
    );
}

/// Report a trap the kernel cannot continue past, and stop the machine.
fn fatal(
    frame: &arch::TrapFrame,
    what: &str,
    entry: &'static crate::panic::catalog::Explanation,
) -> ! {
    report(frame, format_args!("{what}"), entry)
}

/// Report a trap under `headline`, with the registers it saved, and stop.
///
/// The opening lines are the trap's own. Everything after them — the
/// processor, stopping the others, the trace, the explanation, the screen — is
/// what every failure report has, and comes from `panic.rs`, so a fatal trap
/// is drawn on the framebuffer as a panic is. One marker line, not two: the
/// page fault's description is the headline, not a line above it.
fn report(
    frame: &arch::TrapFrame,
    headline: core::fmt::Arguments<'_>,
    entry: &'static crate::panic::catalog::Explanation,
) -> ! {
    let first = crate::panic::begin_report();
    println!();
    println!("FERRIX-PANIC {headline}");
    arch::report_trap(frame);
    if !first {
        crate::panic::abridged()
    }
    crate::panic::conclude(Some(entry))
}
