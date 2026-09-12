//! What happens when the kernel cannot go on.
//!
//! A fatal condition in the kernel is a `panic!` with a sentence saying what
//! went wrong: a self-check in `kmain` that failed, a processor that never
//! answered, a secondary that arrived with no record of its own. The crate
//! root argues why `panic!` is exempt from its lint here. This module is the
//! other half, the report, so that no site prints a marker and halts by hand
//! and every failure arrives in the same shape:
//!
//! ```text
//! FERRIX-PANIC stage 3 self-check failed: the timer never fired
//!   at        kernel/src/main.rs:152:23
//!   on        processor 0 (hardware id 0x0)
//!   stopped   this processor halts here, and 3 more were asked to
//!   trace     #0  0xffffffff800018e7
//!   trace     #1  0xffffffff8000812c
//! ```
//!
//! The trace is addresses only. `xtask` appends the function each one is in,
//! from the kernel ELF it booted, as the lines arrive.
//!
//! The marker and the message share the first line because that is the line
//! the boot test judges on. `xtask` keeps reading for a moment after it, so the
//! lines below it reach the log as well.
//!
//! # Not making it worse
//!
//! Everything beyond printing the message is a chance to fault again, on a
//! machine already in a state nobody planned for. So the processor is named
//! from a register compared against the processor table, never followed; and a
//! second panic — from inside this report, or on another processor while it is
//! being written — prints its message and location and nothing else.

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::console::{self, println};
use crate::{arch, backtrace, smp};

/// Whether a panic report has begun, on any processor.
static REPORTING: AtomicBool = AtomicBool::new(false);

/// Where every kernel panic ends up.
///
/// There is no supervisor above us and no unwinder, so this says what happened
/// and stops the processor it is running on.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    // Before anything else, and before the other processors are asked to
    // stop: an inter-processor interrupt taken part way through this report
    // would halt the one processor that is writing it.
    arch::disable_interrupts();
    console::begin_panic();
    let first = !REPORTING.swap(true, Ordering::AcqRel);

    println!();
    println!("FERRIX-PANIC {}", info.message());
    if let Some(location) = info.location() {
        println!("  at        {location}");
    }
    if !first {
        println!("  stopped   during another panic's report, so this one is abridged");
        arch::halt()
    }

    match smp::this_cpu_for_report() {
        Ok(cpu) => println!(
            "  on        processor {} (hardware id {:#x})",
            cpu.logical, cpu.hardware_id
        ),
        Err(which) => println!("  on        {which}"),
    }
    let others = smp::stop_others();
    if others == 0 {
        println!("  stopped   this processor halts here; nothing will recover it");
    } else {
        println!("  stopped   this processor halts here, and {others} more were asked to");
    }
    report_backtrace();
    arch::halt()
}

/// Print the chain of calls that reached the panic.
///
/// Addresses only: the kernel carries no symbol table, and a panic is the
/// worst moment to go looking for one. `xtask` names them from the image it
/// booted as the report arrives.
fn report_backtrace() {
    let mut index = 0;
    let found = backtrace::walk(arch::frame_pointer(), |address| {
        println!("  trace     #{index:<2} {address:#018x}");
        index += 1;
    });
    if found == 0 {
        println!("  trace     no frame pointer chain to follow from here");
    } else if found == backtrace::MAX_FRAMES {
        println!("  trace     ... stopped at {found} frames");
    }
}
