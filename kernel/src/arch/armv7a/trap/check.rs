//! A program's own exceptions end it with the signal Linux gives each.
//!
//! Verification, not the handlers: a file of its own so that the manifest
//! counts it as the test it is (`scripts/certification-item.json`,
//! `test_file_patterns`). Each case is a real program, built into an ELF and
//! run by the Linux personality's loader, because only an instruction a
//! program executes in USR mode takes the path under test: the vector stub,
//! `classify`, the generic dispatcher's user fault and `fault_signal`, and
//! the kill that ends the program with the signal. `super::super::check`
//! drives the decoder with every vector entry and fault status from built
//! frames; this is the same decoding reached the way a program reaches it,
//! for the cases a program can raise on any core.
//!
//! Before this, the one program the boot ended by a fault wrote through a
//! null pointer (`object/check.rs`), so an undefined instruction, a `bkpt`
//! and an alignment fault a program raises itself had never been taken from
//! USR mode. The last program installs a signal handler without a restorer,
//! whose frame nothing else builds.

use ferrix_linux_abi::types::{SIGBUS, SIGILL, SIGTRAP};

use crate::console::println;

/// `udf #0` in the ARM instruction set: the undefined-instruction vector.
///
/// ```text
///   udf #0
///   mov r7, #248 ; mov r0, #97 ; svc #0             ; exit_group(97)
/// ```
///
/// Every program here ends with that `exit_group(97)`, which only a program
/// the exception did not end reaches. Encoded by hand from the ARM ARM's
/// tables, one little-endian word per instruction.
const UNDEFINED: &[u8] = &[
    0xf0, 0x00, 0xf0, 0xe7, 0xf8, 0x70, 0xa0, 0xe3, 0x61, 0x00, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
];

/// `bkpt #0`: a prefetch abort with the debug-event status.
const BREAKPOINT: &[u8] = &[
    0x70, 0x00, 0x20, 0xe1, 0xf8, 0x70, 0xa0, 0xe3, 0x61, 0x00, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
];

/// `ldm` from one byte past the stack pointer: a load-multiple takes an
/// alignment fault at any address that is not a word's, whatever `SCTLR.A`
/// says, and the stack is mapped, so no other fault can come first.
///
/// ```text
///   mov r0, sp ; add r0, r0, #1 ; ldm r0, {r1}
/// ```
const MISALIGNED: &[u8] = &[
    0x0d, 0x00, 0xa0, 0xe1, 0x01, 0x00, 0x80, 0xe2, 0x02, 0x00, 0x90, 0xe8, 0xf8, 0x70, 0xa0, 0xe3,
    0x61, 0x00, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
];

/// A handler installed without `SA_RESTORER`, which the frame then returns
/// into through the copy of the return sequence it carries on the stack
/// (`super::super::signal`): the handler is entered, and exits with 55.
///
/// ```text
///   sub sp, sp, #24 ; adr r0, handler ; str r0, [sp]       ; sa_handler
///   mov r0, #0 ; str r0, [sp, #4..#16]                      ; no flags, no restorer, empty mask
///   rt_sigaction(SIGUSR1, sp, 0, 8) ; bne fail
///   kill(getpid(), SIGUSR1)
/// fail: exit_group(97)
/// handler: exit_group(55)
/// ```
///
/// Assembled by GNU `as` for ARMv7-A and read back out of the object file.
/// musl, and every program the images carry, installs its handlers with a
/// restorer, so no other program takes this frame.
const NO_RESTORER: &[u8] = &[
    0x18, 0xd0, 0x4d, 0xe2, 0x54, 0x00, 0x8f, 0xe2, 0x00, 0x00, 0x8d, 0xe5, 0x00, 0x00, 0xa0, 0xe3,
    0x04, 0x00, 0x8d, 0xe5, 0x08, 0x00, 0x8d, 0xe5, 0x0c, 0x00, 0x8d, 0xe5, 0x10, 0x00, 0x8d, 0xe5,
    0x0a, 0x00, 0xa0, 0xe3, 0x0d, 0x10, 0xa0, 0xe1, 0x00, 0x20, 0xa0, 0xe3, 0x08, 0x30, 0xa0, 0xe3,
    0xae, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3, 0x04, 0x00, 0x00, 0x1a,
    0x14, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x0a, 0x10, 0xa0, 0xe3, 0x25, 0x70, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xef, 0x61, 0x00, 0xa0, 0xe3, 0xf8, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
    0x37, 0x00, 0xa0, 0xe3, 0xf8, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
];

/// The status of a program a signal ended, as `wait4` reports it to a shell.
const fn killed_by(signal: u32) -> i32 {
    128 + signal as i32
}

/// Run every case, and say what each ended with.
///
/// # Errors
///
/// The first program that could not be run, or ended other than it had to.
pub(crate) fn run() -> Result<(), &'static str> {
    let cases: [(&[u8], &[u8], i32, &'static str); 3] = [
        (
            b"/undefined",
            UNDEFINED,
            killed_by(SIGILL),
            "a program's undefined instruction did not end it with SIGILL",
        ),
        (
            b"/breakpoint",
            BREAKPOINT,
            killed_by(SIGTRAP),
            "a program's own breakpoint did not end it with SIGTRAP",
        ),
        (
            b"/misaligned",
            MISALIGNED,
            killed_by(SIGBUS),
            "a program's misaligned load-multiple did not end it with SIGBUS",
        ),
    ];
    for (name, code, expected, problem) in cases {
        let status = run_program(name, code)?;
        if status != expected {
            println!(
                "  fault    {} ended with {status}, not {expected}",
                core::str::from_utf8(name).unwrap_or("?")
            );
            return Err(problem);
        }
    }
    println!(
        "  fault    a program's undefined instruction, own breakpoint and misaligned \
         load-multiple ended it with SIGILL, SIGTRAP and SIGBUS"
    );

    let status = run_program(b"/no-restorer", NO_RESTORER)?;
    if status != 55 {
        println!("  signal   /no-restorer ended with {status}, not 55");
        return Err("a handler installed without a restorer was not entered");
    }
    println!("  signal   a handler installed without SA_RESTORER was entered from its frame");
    Ok(())
}

/// Build `code` into a program called `name`, run it, and answer its status.
fn run_program(name: &[u8], code: &[u8]) -> Result<i32, &'static str> {
    let file = crate::syscall::image::build_with(
        ferrix_elf::Class::Elf32,
        super::super::ARCH.elf_machine(),
        crate::syscall::image::Shape::Good,
        code,
    );
    crate::syscall::exec::run(&file, &[name], &[], [0x7e; ferrix_ustack::RANDOM_BYTES])
        .map_err(|_| "a program that faults on purpose could not be started")
}
