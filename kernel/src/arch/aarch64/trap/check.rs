//! A program's own exceptions end it with the signal Linux gives each.
//!
//! Verification, not the handlers: a file of its own so that the manifest
//! counts it as the test it is (`scripts/certification-item.json`,
//! `test_file_patterns`). Each case is a real program, built into an ELF and
//! run by the Linux personality's loader, because only an instruction a
//! program executes at EL0 takes the path under test: the vector stub,
//! `classify`, the generic dispatcher's user fault and `fault_signal`, and
//! the kill that ends the program with the signal. `super::super::check`
//! drives the decoder with every syndrome from built frames; this is the
//! same decoding reached the way a program reaches it, for the cases a
//! program can raise on any core.
//!
//! Before this, the one program the boot ended by a fault wrote through a
//! null pointer (`object/check.rs`), so a misaligned program counter, an
//! undefined instruction and a `brk` a program executes itself had never
//! been taken from EL0.

use ferrix_linux_abi::types::{SIGBUS, SIGILL, SIGTRAP};

use crate::console::println;

/// `br` to an address two bytes into this program: the next fetch is from a
/// misaligned program counter, exception class `0b100010`.
///
/// ```text
///   adr x0, . ; add x0, x0, #2 ; br x0
///   mov x8, #94 ; mov x0, #97 ; svc #0              ; exit_group(97)
/// ```
///
/// Every program here ends with that `exit_group(97)`, which only a program
/// the exception did not end reaches. Encoded by hand from the Arm ARM's
/// tables, one little-endian word per instruction.
const MISALIGNED_PC: &[u8] = &[
    0x00, 0x00, 0x00, 0x10, 0x00, 0x08, 0x00, 0x91, 0x00, 0x00, 0x1f, 0xd6, 0xc8, 0x0b, 0x80, 0xd2,
    0x20, 0x0c, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
];

/// `udf #0`: an unallocated encoding, exception class `0b000000`.
const UNDEFINED: &[u8] = &[
    0x00, 0x00, 0x00, 0x00, 0xc8, 0x0b, 0x80, 0xd2, 0x20, 0x0c, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
];

/// `brk #0` from EL0: exception class `0b111100`, which the kernel steps over
/// when it raised it itself and turns into a signal when a program did.
const BREAKPOINT: &[u8] = &[
    0x00, 0x00, 0x20, 0xd4, 0xc8, 0x0b, 0x80, 0xd2, 0x20, 0x0c, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
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
            b"/misaligned",
            MISALIGNED_PC,
            killed_by(SIGBUS),
            "a program's misaligned program counter did not end it with SIGBUS",
        ),
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
        "  fault    a program's misaligned program counter, undefined instruction and own \
         breakpoint ended it with SIGBUS, SIGILL and SIGTRAP"
    );
    Ok(())
}

/// Build `code` into a program called `name`, run it, and answer its status.
fn run_program(name: &[u8], code: &[u8]) -> Result<i32, &'static str> {
    let file = crate::syscall::image::build_with(
        ferrix_elf::Class::Elf64,
        super::super::ARCH.elf_machine(),
        crate::syscall::image::Shape::Good,
        code,
    );
    crate::syscall::exec::run(&file, &[name], &[], [0x7e; ferrix_ustack::RANDOM_BYTES])
        .map_err(|_| "a program that faults on purpose could not be started")
}
