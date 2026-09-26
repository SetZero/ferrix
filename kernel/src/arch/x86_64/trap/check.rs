//! A program's own exceptions end it with the signal Linux gives each, and
//! `arch_prctl` refuses what it must.
//!
//! Verification, not the handlers: a file of its own so that the manifest
//! counts it as the test it is (`scripts/certification-item.json`,
//! `test_file_patterns`). Each case is a real program, built into an ELF and
//! run by the Linux personality's loader, because only an instruction a
//! program executes in ring 3 takes the path under test: the stub, `classify`,
//! the generic dispatcher's user fault and `fault_signal`, and the kill that
//! ends the program with the signal. The programs are fixtures, and live here
//! rather than beside the product's in `super::super`.
//!
//! Before this, the one program the boot ended by a fault wrote through a null
//! pointer (`object/check.rs`), so a divide error, an invalid opcode, a
//! privileged instruction and a floating-point exception each reached a line
//! of `fault_signal` no boot had run, and nothing had touched a file mapping
//! past its file's end, which the dispatcher turns into `SIGBUS` itself.

use ferrix_linux_abi::types::{SIGBUS, SIGFPE, SIGILL, SIGSEGV};

use super::super::cpu;
use crate::console::println;

/// `divl %ecx` with `%ecx` zero: `#DE`, vector 0.
///
/// ```text
///   xorl %ecx, %ecx ; movl $1, %eax ; xorl %edx, %edx
///   divl %ecx                                  ; #DE
///   movl $231, %eax ; movl $97, %edi ; syscall ; exit_group(97)
/// ```
///
/// Every program here ends with that `exit_group(97)`, which only a program
/// the exception did not end reaches. Assembled by GNU `as` and read back
/// out of the object file.
const DIVIDE: &[u8] = &[
    0x31, 0xc9, 0xb8, 0x01, 0x00, 0x00, 0x00, 0x31, 0xd2, 0xf7, 0xf1, 0xb8, 0xe7, 0x00, 0x00, 0x00,
    0xbf, 0x61, 0x00, 0x00, 0x00, 0x0f, 0x05,
];

/// `ud2`: `#UD`, vector 6.
const INVALID: &[u8] = &[
    0x0f, 0x0b, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x61, 0x00, 0x00, 0x00, 0x0f, 0x05,
];

/// A read of an address no mapping covers: `#PF`, vector 14, with the present
/// bit clear. The null write `object/check.rs` makes is the other case.
///
/// ```text
///   movabsq $0x100000000000, %rax ; movq (%rax), %rax
/// ```
const UNMAPPED: &[u8] = &[
    0x48, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x48, 0x8b, 0x00, 0xb8, 0xe7, 0x00,
    0x00, 0x00, 0xbf, 0x61, 0x00, 0x00, 0x00, 0x0f, 0x05,
];

/// `hlt` in ring 3: `#GP`, vector 13, which `classify` has no case of its
/// own for and hands on by name.
const PRIVILEGED: &[u8] = &[
    0xf4, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x61, 0x00, 0x00, 0x00, 0x0f, 0x05,
];

/// `1.0 / 0.0` on the x87 with the zero-divide exception unmasked, and an
/// `fwait` to deliver it: `#MF`, vector 16, where `CR0.NE` asks for native
/// reporting, as every firmware since the 486 leaves it.
///
/// ```text
///   fninit ; subq $8, %rsp ; fnstcw (%rsp) ; andw $0xfffb, (%rsp) ; fldcw (%rsp)
///   fldz ; fld1 ; fdivrp %st, %st(1)           ; 1.0 / 0.0, pending
///   fwait                                      ; #MF
/// ```
///
/// The x87 rather than SSE's `#XM`, which is the more common case: QEMU's
/// TCG does not raise SIMD floating-point exceptions at all, and the program
/// ran on to its `exit_group(97)`.
const X87: &[u8] = &[
    0xdb, 0xe3, 0x48, 0x83, 0xec, 0x08, 0xd9, 0x3c, 0x24, 0x66, 0x83, 0x24, 0x24, 0xfb, 0xd9, 0x2c,
    0x24, 0xd9, 0xee, 0xd9, 0xe8, 0xde, 0xf1, 0x9b, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x61, 0x00,
    0x00, 0x00, 0x0f, 0x05,
];

/// A read of the second page of a shared mapping of a one-page file: `#PF`,
/// which the address space answers with the file's end rather than a missing
/// mapping, so the dispatcher sends `SIGBUS` and not what `fault_signal` would
/// say. Exits with 96 if the file or the mapping could not be made.
///
/// ```text
///   leaq name(%rip), %rdi ; xorl %esi, %esi ; movl $319, %eax ; syscall
///   testq %rax, %rax ; js 1f ; movq %rax, %r8           ; memfd_create
///   movq %rax, %rdi ; movl $4096, %esi ; movl $77, %eax ; syscall
///   testq %rax, %rax ; jnz 1f                           ; ftruncate to a page
///   xorl %edi, %edi ; movl $8192, %esi ; movl $1, %edx ; movl $1, %r10d
///   xorl %r9d, %r9d ; movl $9, %eax ; syscall            ; two pages, shared
///   cmpq $-4096, %rax ; ja 1f
///   movq 4096(%rax), %rax                                ; past the file
///   movl $231, %eax ; movl $97, %edi ; syscall
/// 1: movl $231, %eax ; movl $96, %edi ; syscall
/// name: .asciz "bus"
/// ```
const PAST_END: &[u8] = &[
    0x48, 0x8d, 0x3d, 0x68, 0x00, 0x00, 0x00, 0x31, 0xf6, 0xb8, 0x3f, 0x01, 0x00, 0x00, 0x0f, 0x05,
    0x48, 0x85, 0xc0, 0x78, 0x4e, 0x49, 0x89, 0xc0, 0x48, 0x89, 0xc7, 0xbe, 0x00, 0x10, 0x00, 0x00,
    0xb8, 0x4d, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x85, 0xc0, 0x75, 0x37, 0x31, 0xff, 0xbe, 0x00,
    0x20, 0x00, 0x00, 0xba, 0x01, 0x00, 0x00, 0x00, 0x41, 0xba, 0x01, 0x00, 0x00, 0x00, 0x45, 0x31,
    0xc9, 0xb8, 0x09, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x3d, 0x00, 0xf0, 0xff, 0xff, 0x77, 0x13,
    0x48, 0x8b, 0x80, 0x00, 0x10, 0x00, 0x00, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x61, 0x00, 0x00,
    0x00, 0x0f, 0x05, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x60, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x62,
    0x75, 0x73, 0x00,
];

/// `arch_prctl(ARCH_SET_FS, 0xffff800000000000)` has to be `EPERM` and
/// `arch_prctl(ARCH_GET_FS, 0)` `EINVAL`; the program exits with 0 when both
/// are, 1 when the first is not and 2 when the second is not.
///
/// ```text
///   movl $158, %eax ; movl $0x1002, %edi ; movabsq $0xffff800000000000, %rsi
///   syscall ; cmpq $-1, %rax ; jne 1f
///   movl $158, %eax ; movl $0x1003, %edi ; xorl %esi, %esi
///   syscall ; cmpq $-22, %rax ; jne 2f
///   movl $231, %eax ; xorl %edi, %edi ; syscall
/// 1: movl $231, %eax ; movl $1, %edi ; syscall
/// 2: movl $231, %eax ; movl $2, %edi ; syscall
/// ```
const PRCTL: &[u8] = &[
    0xb8, 0x9e, 0x00, 0x00, 0x00, 0xbf, 0x02, 0x10, 0x00, 0x00, 0x48, 0xbe, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x80, 0xff, 0xff, 0x0f, 0x05, 0x48, 0x83, 0xf8, 0xff, 0x75, 0x1d, 0xb8, 0x9e, 0x00, 0x00,
    0x00, 0xbf, 0x03, 0x10, 0x00, 0x00, 0x31, 0xf6, 0x0f, 0x05, 0x48, 0x83, 0xf8, 0xea, 0x75, 0x15,
    0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31, 0xff, 0x0f, 0x05, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x01,
    0x00, 0x00, 0x00, 0x0f, 0x05, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x02, 0x00, 0x00, 0x00, 0x0f,
    0x05,
];

/// `CR0.NE`: x87 exceptions are reported as `#MF`, not through the legacy
/// `FERR#` line to an interrupt controller.
const CR0_NE: u64 = 1 << 5;

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
    if cpu::read_cr0() & CR0_NE == 0 {
        return Err("CR0.NE is clear, so an x87 exception is not an exception");
    }
    let cases: [(&[u8], &[u8], i32, &'static str); 6] = [
        (
            b"/divide",
            DIVIDE,
            killed_by(SIGFPE),
            "a program's divide error did not end it with SIGFPE",
        ),
        (
            b"/invalid",
            INVALID,
            killed_by(SIGILL),
            "a program's invalid opcode did not end it with SIGILL",
        ),
        (
            b"/unmapped",
            UNMAPPED,
            killed_by(SIGSEGV),
            "a program's read of an unmapped address did not end it with SIGSEGV",
        ),
        (
            b"/privileged",
            PRIVILEGED,
            killed_by(SIGSEGV),
            "a program's privileged instruction did not end it with SIGSEGV",
        ),
        (
            b"/x87",
            X87,
            killed_by(SIGFPE),
            "a program's x87 floating-point exception did not end it with SIGFPE",
        ),
        (
            b"/past-end",
            PAST_END,
            killed_by(SIGBUS),
            "a program's read past the end of a mapped file did not end it with SIGBUS",
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
        "  fault    a program's divide error, invalid opcode, unmapped read, privileged \
         instruction, x87 exception and read past a mapped file's end ended it with SIGFPE, \
         SIGILL, SIGSEGV, SIGSEGV, SIGFPE and SIGBUS"
    );

    match run_program(b"/prctl", PRCTL)? {
        0 => {}
        1 => return Err("arch_prctl accepted a kernel address as the thread pointer"),
        _ => return Err("arch_prctl answered an unknown request with something but EINVAL"),
    }
    println!(
        "  prctl    arch_prctl refused a kernel thread pointer with EPERM and ARCH_GET_FS with \
         EINVAL"
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
