//! A program's own exceptions end it with the signal Linux gives each,
//! `arch_prctl` refuses what it must, and compatibility mode is entered and
//! left only the ways `docs/I386.md` §3.3 allows.
//!
//! Verification, not the handlers: a file of its own so that the manifest
//! counts it as the test it is (`scripts/data/certification-item.json`,
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

/// An i386 program, in an `ELFCLASS32` `EM_386` image: it writes a line
/// through `int $0x80`, and exits with 42 only if the write returned its
/// length, a write to descriptor -1 returned `-EBADF` whole in `EAX`, and
/// number 39 -- i386's `mkdir`, which nobody has read for 32-bit widths yet
/// -- answered `-ENOSYS` (`docs/I386.md` §3.2). 43 otherwise.
///
/// ```text
///   movl $4, %eax ; movl $1, %ebx ; call 1f
/// 1: popl %ecx ; addl $(msg - 1b), %ecx ; movl $16, %edx ; int $0x80
///   cmpl $16, %eax ; jne 9f                     ; write(1, msg, 16)
///   movl $4, %eax ; movl $-1, %ebx ; int $0x80
///   cmpl $-9, %eax ; jne 9f                     ; write(-1): EBADF
///   movl $39, %eax ; int $0x80
///   cmpl $-38, %eax ; jne 9f                    ; unmapped: ENOSYS
///   movl $252, %eax ; movl $42, %ebx ; int $0x80 ; exit_group(42)
/// 9: movl $252, %eax ; movl $43, %ebx ; int $0x80
/// msg: .ascii "hello from i386\n"
/// ```
///
/// Assembled by GNU `as` in `.code32`, linked at the image's entry, and read
/// back with `objdump -m i386`.
const HELLO_I386: &[u8] = &[
    0xb8, 0x04, 0x00, 0x00, 0x00, 0xbb, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x00, 0x00, 0x00, 0x00, 0x59,
    0x81, 0xc1, 0x4a, 0x00, 0x00, 0x00, 0xba, 0x10, 0x00, 0x00, 0x00, 0xcd, 0x80, 0x3d, 0x10, 0x00,
    0x00, 0x00, 0x75, 0x29, 0xb8, 0x04, 0x00, 0x00, 0x00, 0xbb, 0xff, 0xff, 0xff, 0xff, 0xcd, 0x80,
    0x83, 0xf8, 0xf7, 0x75, 0x18, 0xb8, 0x27, 0x00, 0x00, 0x00, 0xcd, 0x80, 0x83, 0xf8, 0xda, 0x75,
    0x0c, 0xb8, 0xfc, 0x00, 0x00, 0x00, 0xbb, 0x2a, 0x00, 0x00, 0x00, 0xcd, 0x80, 0xb8, 0xfc, 0x00,
    0x00, 0x00, 0xbb, 0x2b, 0x00, 0x00, 0x00, 0xcd, 0x80, b'h', b'e', b'l', b'l', b'o', b' ', b'f',
    b'r', b'o', b'm', b' ', b'i', b'3', b'8', b'6', b'\n',
];

/// What [`HELLO_I386`] exits with when every call answered as it had to.
const HELLO_I386_STATUS: i32 = 42;

/// ELF's `e_machine` for i386.
const EM_386: u16 = 3;

/// A 64-bit program's `int $0x80` is an i386 call, as on Linux: 20 is
/// `getpid` there (and `writev` in x86-64's table), and must answer what a
/// `SYSCALL` `getpid` does. Then `dup` of `0x1_0000_0001`, whose upper half a
/// 32-bit program could never have set, must duplicate descriptor 1, and the
/// copy close. Exits 0, or 1 if `getpid` disagreed, or 2 if the upper half
/// was read.
///
/// ```text
///   movl $39, %eax ; syscall ; movq %rax, %r12  ; getpid, x86-64's number
///   movl $20, %eax ; int $0x80                  ; getpid, i386's number
///   cmpq %rax, %r12 ; jne 1f
///   movabsq $0x100000001, %rbx ; movl $41, %eax ; int $0x80   ; dup(1)
///   testq %rax, %rax ; js 2f
///   movl %eax, %ebx ; movl $6, %eax ; int $0x80 ; testq %rax, %rax ; jnz 2f
///   movl $231, %eax ; xorl %edi, %edi ; syscall
/// 1: movl $231, %eax ; movl $1, %edi ; syscall
/// 2: movl $231, %eax ; movl $2, %edi ; syscall
/// ```
const INT80_FROM_64: &[u8] = &[
    0xb8, 0x27, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x49, 0x89, 0xc4, 0xb8, 0x14, 0x00, 0x00, 0x00, 0xcd,
    0x80, 0x49, 0x39, 0xc4, 0x75, 0x2d, 0x48, 0xbb, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0xb8, 0x29, 0x00, 0x00, 0x00, 0xcd, 0x80, 0x48, 0x85, 0xc0, 0x78, 0x23, 0x89, 0xc3, 0xb8, 0x06,
    0x00, 0x00, 0x00, 0xcd, 0x80, 0x48, 0x85, 0xc0, 0x75, 0x15, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31,
    0xff, 0x0f, 0x05, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xb8,
    0xe7, 0x00, 0x00, 0x00, 0xbf, 0x02, 0x00, 0x00, 0x00, 0x0f, 0x05,
];

/// A 64-bit program far-returns into the 32-bit code segment and issues
/// `SYSCALL` there, which goes to `IA32_CSTAR` on AMD -- and under QEMU's
/// TCG -- and is `#UD` on Intel. Either way it must not enter the kernel
/// anywhere but the stub that answers `-ENOSYS` (`docs/I386.md` §3.3). Back
/// in 64-bit mode by a far jump, it exits 0 for `-ENOSYS` and 1 for anything
/// else; `SIGILL` ends it on Intel.
///
/// ```text
/// .code64: pushq $0x23 ; pushq $compat ; lretq
/// .code32: compat: syscall ; movl %eax, %esi ; ljmp $0x33, $back
/// .code64: back: xorl %edi, %edi ; cmpl $-38, %esi ; je 1f ; movl $1, %edi
///          1: movl $231, %eax ; syscall
/// ```
///
/// Assembled by GNU `as`, linked at the image's entry, and read back in each
/// half's own mode.
const COMPAT_SYSCALL: &[u8] = &[
    0x6a, 0x23, 0x68, 0x09, 0x01, 0x40, 0x00, 0x48, 0xcb, 0x0f, 0x05, 0x89, 0xc6, 0xea, 0x14, 0x01,
    0x40, 0x00, 0x33, 0x00, 0x31, 0xff, 0x83, 0xfe, 0xda, 0x74, 0x05, 0xbf, 0x01, 0x00, 0x00, 0x00,
    0xb8, 0xe7, 0x00, 0x00, 0x00, 0x0f, 0x05,
];

/// The same far return, then `SYSENTER`: `#GP` with `IA32_SYSENTER_CS` zero
/// on Intel and under TCG, `#UD` on AMD, which has no `SYSENTER` in long mode.
/// Either ends the program with a signal; reaching `exit_group(97)` means it
/// entered the kernel somewhere and came back.
///
/// ```text
/// .code64: pushq $0x23 ; pushq $compat ; lretq
/// .code32: compat: sysenter ; ljmp $0x33, $back
/// .code64: back: movl $231, %eax ; movl $97, %edi ; syscall
/// ```
const COMPAT_SYSENTER: &[u8] = &[
    0x6a, 0x23, 0x68, 0x09, 0x01, 0x40, 0x00, 0x48, 0xcb, 0x0f, 0x34, 0xea, 0x12, 0x01, 0x40, 0x00,
    0x33, 0x00, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x61, 0x00, 0x00, 0x00, 0x0f, 0x05,
];

/// An i386 program's thread pointer, as musl sets one up: `set_thread_area`
/// with entry -1, base the image's data page, limit `0xfffff`, flags `0x51`;
/// the entry chosen, 12, loaded into `%gs` as selector `0x63`. It then reads
/// `%gs:0` two hundred times with a `sched_yield` between each, and must
/// see its own base's word every time -- [`TLS_I386_HIGH`] runs the same
/// program with a base four bytes on, pinned to the same processor, so a
/// descriptor or selector that did not travel with its thread shows as the
/// other's word. Then `get_thread_area(12)` must give the base back, a
/// second entry -1 must be 13, and a code segment must be `EINVAL`. Exits
/// 42, or 40 to 46 for the step that failed: the call, the entry chosen, the
/// read back, the second entry, the refusal, the word through `%gs`.
///
/// ```text
///   subl $16, %esp ; user_desc { -1, BASE, 0xfffff, 0x51 } at (%esp)
///   movl $243, %eax ; movl %esp, %ebx ; int $0x80        ; set_thread_area
///   movl $40, %ebx ; testl %eax, %eax ; jnz 9f
///   movl (%esp), %edx ; movl $41, %ebx ; cmpl $12, %edx ; jne 9f
///   leal 3(,%edx,8), %edx ; movw %dx, %gs ; movl $200, %esi
/// 1: movl %gs:0, %eax ; movl $46, %ebx ; cmpl $WANT, %eax ; jne 9f
///   movl $158, %eax ; int $0x80 ; decl %esi ; jnz 1b      ; sched_yield
///   entry 12 at (%esp) ; movl $244, %eax ; movl %esp, %ebx ; int $0x80
///   movl $43, %ebx ; testl %eax, %eax ; jnz 9f ; cmpl $BASE, 4(%esp) ; jne 9f
///   user_desc { -1, BASE, 0xfffff, 0x51 } ; set_thread_area
///   movl $44, %ebx ; cmpl $13, (%esp) ; jne 9f
///   entry -1, flags 0x55 (code) ; set_thread_area
///   movl $45, %ebx ; cmpl $-22, %eax ; jne 9f
///   movl $42, %ebx
/// 9: movl $252, %eax ; int $0x80                          ; exit_group(%ebx)
/// ```
///
/// This one's `BASE` is the data page, `0x410000`, whose first word is
/// `"stag"` of `DATA_MARK`. Assembled by GNU `as --32` twice with the two
/// `--defsym` pairs, linked at the image's entry, read back with
/// `objdump -m i386`; the two differ in seven bytes.
const TLS_I386_LOW: &[u8] = &[
    0x83, 0xec, 0x10, 0xc7, 0x04, 0x24, 0xff, 0xff, 0xff, 0xff, 0xc7, 0x44, 0x24, 0x04, 0x00, 0x00,
    0x41, 0x00, 0xc7, 0x44, 0x24, 0x08, 0xff, 0xff, 0x0f, 0x00, 0xc7, 0x44, 0x24, 0x0c, 0x51, 0x00,
    0x00, 0x00, 0xb8, 0xf3, 0x00, 0x00, 0x00, 0x89, 0xe3, 0xcd, 0x80, 0xbb, 0x28, 0x00, 0x00, 0x00,
    0x85, 0xc0, 0x0f, 0x85, 0xc4, 0x00, 0x00, 0x00, 0x8b, 0x14, 0x24, 0xbb, 0x29, 0x00, 0x00, 0x00,
    0x83, 0xfa, 0x0c, 0x0f, 0x85, 0xb3, 0x00, 0x00, 0x00, 0x8d, 0x14, 0xd5, 0x03, 0x00, 0x00, 0x00,
    0x8e, 0xea, 0xbe, 0xc8, 0x00, 0x00, 0x00, 0x65, 0xa1, 0x00, 0x00, 0x00, 0x00, 0xbb, 0x2e, 0x00,
    0x00, 0x00, 0x3d, 0x73, 0x74, 0x61, 0x67, 0x0f, 0x85, 0x8f, 0x00, 0x00, 0x00, 0xb8, 0x9e, 0x00,
    0x00, 0x00, 0xcd, 0x80, 0x4e, 0x75, 0xe0, 0xc7, 0x04, 0x24, 0x0c, 0x00, 0x00, 0x00, 0xc7, 0x44,
    0x24, 0x04, 0x00, 0x00, 0x00, 0x00, 0xb8, 0xf4, 0x00, 0x00, 0x00, 0x89, 0xe3, 0xcd, 0x80, 0xbb,
    0x2b, 0x00, 0x00, 0x00, 0x85, 0xc0, 0x75, 0x64, 0x81, 0x7c, 0x24, 0x04, 0x00, 0x00, 0x41, 0x00,
    0x75, 0x5a, 0xc7, 0x04, 0x24, 0xff, 0xff, 0xff, 0xff, 0xc7, 0x44, 0x24, 0x04, 0x00, 0x00, 0x41,
    0x00, 0xc7, 0x44, 0x24, 0x08, 0xff, 0xff, 0x0f, 0x00, 0xc7, 0x44, 0x24, 0x0c, 0x51, 0x00, 0x00,
    0x00, 0xb8, 0xf3, 0x00, 0x00, 0x00, 0x89, 0xe3, 0xcd, 0x80, 0xbb, 0x2c, 0x00, 0x00, 0x00, 0x83,
    0x3c, 0x24, 0x0d, 0x75, 0x27, 0xc7, 0x04, 0x24, 0xff, 0xff, 0xff, 0xff, 0xc7, 0x44, 0x24, 0x0c,
    0x55, 0x00, 0x00, 0x00, 0xb8, 0xf3, 0x00, 0x00, 0x00, 0x89, 0xe3, 0xcd, 0x80, 0xbb, 0x2d, 0x00,
    0x00, 0x00, 0x83, 0xf8, 0xea, 0x75, 0x05, 0xbb, 0x2a, 0x00, 0x00, 0x00, 0xb8, 0xfc, 0x00, 0x00,
    0x00, 0xcd, 0x80,
];

/// [`TLS_I386_LOW`] with `BASE` `0x410004`, whose word is `"e7-l"`.
const TLS_I386_HIGH: &[u8] = &[
    0x83, 0xec, 0x10, 0xc7, 0x04, 0x24, 0xff, 0xff, 0xff, 0xff, 0xc7, 0x44, 0x24, 0x04, 0x04, 0x00,
    0x41, 0x00, 0xc7, 0x44, 0x24, 0x08, 0xff, 0xff, 0x0f, 0x00, 0xc7, 0x44, 0x24, 0x0c, 0x51, 0x00,
    0x00, 0x00, 0xb8, 0xf3, 0x00, 0x00, 0x00, 0x89, 0xe3, 0xcd, 0x80, 0xbb, 0x28, 0x00, 0x00, 0x00,
    0x85, 0xc0, 0x0f, 0x85, 0xc4, 0x00, 0x00, 0x00, 0x8b, 0x14, 0x24, 0xbb, 0x29, 0x00, 0x00, 0x00,
    0x83, 0xfa, 0x0c, 0x0f, 0x85, 0xb3, 0x00, 0x00, 0x00, 0x8d, 0x14, 0xd5, 0x03, 0x00, 0x00, 0x00,
    0x8e, 0xea, 0xbe, 0xc8, 0x00, 0x00, 0x00, 0x65, 0xa1, 0x00, 0x00, 0x00, 0x00, 0xbb, 0x2e, 0x00,
    0x00, 0x00, 0x3d, 0x65, 0x37, 0x2d, 0x6c, 0x0f, 0x85, 0x8f, 0x00, 0x00, 0x00, 0xb8, 0x9e, 0x00,
    0x00, 0x00, 0xcd, 0x80, 0x4e, 0x75, 0xe0, 0xc7, 0x04, 0x24, 0x0c, 0x00, 0x00, 0x00, 0xc7, 0x44,
    0x24, 0x04, 0x00, 0x00, 0x00, 0x00, 0xb8, 0xf4, 0x00, 0x00, 0x00, 0x89, 0xe3, 0xcd, 0x80, 0xbb,
    0x2b, 0x00, 0x00, 0x00, 0x85, 0xc0, 0x75, 0x64, 0x81, 0x7c, 0x24, 0x04, 0x04, 0x00, 0x41, 0x00,
    0x75, 0x5a, 0xc7, 0x04, 0x24, 0xff, 0xff, 0xff, 0xff, 0xc7, 0x44, 0x24, 0x04, 0x04, 0x00, 0x41,
    0x00, 0xc7, 0x44, 0x24, 0x08, 0xff, 0xff, 0x0f, 0x00, 0xc7, 0x44, 0x24, 0x0c, 0x51, 0x00, 0x00,
    0x00, 0xb8, 0xf3, 0x00, 0x00, 0x00, 0x89, 0xe3, 0xcd, 0x80, 0xbb, 0x2c, 0x00, 0x00, 0x00, 0x83,
    0x3c, 0x24, 0x0d, 0x75, 0x27, 0xc7, 0x04, 0x24, 0xff, 0xff, 0xff, 0xff, 0xc7, 0x44, 0x24, 0x0c,
    0x55, 0x00, 0x00, 0x00, 0xb8, 0xf3, 0x00, 0x00, 0x00, 0x89, 0xe3, 0xcd, 0x80, 0xbb, 0x2d, 0x00,
    0x00, 0x00, 0x83, 0xf8, 0xea, 0x75, 0x05, 0xbb, 0x2a, 0x00, 0x00, 0x00, 0xb8, 0xfc, 0x00, 0x00,
    0x00, 0xcd, 0x80,
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
    // What the trap path does after an `execve`: equal only to the same
    // entry, stack and mode. The dispatcher's callers compare outcomes, and
    // an entry compared by its return value alone would restart a program at
    // another's first instruction, or a 32-bit one in 64-bit mode.
    let entered = crate::trap::Outcome::Enter {
        entry: 0x40_1000,
        stack: 0x7fff_f000,
        abi: crate::trap::Abi::Native,
    };
    let elsewhere = crate::trap::Outcome::Enter {
        entry: 0x40_1000,
        stack: 0x7fff_e000,
        abi: crate::trap::Abi::Native,
    };
    let other_mode = crate::trap::Outcome::Enter {
        entry: 0x40_1000,
        stack: 0x7fff_f000,
        abi: crate::trap::Abi::Compat,
    };
    let again = crate::trap::Outcome::Enter {
        entry: 0x40_1000,
        stack: 0x7fff_f000,
        abi: crate::trap::Abi::Native,
    };
    // Through `black_box`, or the comparison is folded where it is written
    // and the trap path's own equality never runs.
    let entered = core::hint::black_box(entered);
    if entered == core::hint::black_box(elsewhere)
        || entered == core::hint::black_box(other_mode)
        || entered != core::hint::black_box(again)
    {
        return Err("two entries into a program compared by something but entry, stack and mode");
    }

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
    check_compat()
}

/// The ways into and out of compatibility mode (`docs/I386.md` I1): an i386
/// program runs and calls through `int $0x80`, a 64-bit program's `int $0x80`
/// is an i386 call, and neither `SYSCALL` nor `SYSENTER` from 32-bit code
/// enters the kernel anywhere else.
fn check_compat() -> Result<(), &'static str> {
    let file = crate::syscall::image::build_with(
        ferrix_elf::Class::Elf32,
        EM_386,
        crate::syscall::image::Shape::Good,
        HELLO_I386,
    );
    let status =
        crate::syscall::exec::run(&file, &[b"/i386"], &[], [0x7e; ferrix_ustack::RANDOM_BYTES])
            .map_err(|_| "an i386 program could not be started")?;
    if status != HELLO_I386_STATUS {
        println!("  i386     the i386 program ended with {status}, not {HELLO_I386_STATUS}");
        return Err(
            "an i386 program's calls through int $0x80 did not answer as the i386 ABI does",
        );
    }

    match run_program(b"/int80", INT80_FROM_64)? {
        0 => {}
        1 => return Err("a 64-bit program's int $0x80 was not an i386 call"),
        _ => return Err("int $0x80 read the upper half of a 64-bit program's register"),
    }

    let tls = check_thread_areas()?;

    let syscall_ended = run_program(b"/cstar", COMPAT_SYSCALL)?;
    let syscall_how = match syscall_ended {
        0 => "-ENOSYS",
        status if status == killed_by(SIGILL) => "SIGILL",
        _ => return Err("SYSCALL from compatibility mode was neither refused nor #UD"),
    };
    let sysenter_ended = run_program(b"/sysenter", COMPAT_SYSENTER)?;
    let sysenter_how = match sysenter_ended {
        status if status == killed_by(SIGSEGV) => "SIGSEGV",
        status if status == killed_by(SIGILL) => "SIGILL",
        _ => return Err("SYSENTER from compatibility mode did not end the program"),
    };
    println!(
        "  i386     a 32-bit program wrote and exited through int $0x80, and a 64-bit one's int \
         $0x80 was an i386 call; SYSCALL from 32-bit code answered {syscall_how}, SYSENTER \
         {sysenter_how}"
    );
    println!(
        "  i386     2 programs on one processor each read their own thread-local segment \
         through %gs {tls} times across sched_yield, set_thread_area chose entries 12 and 13 \
         and refused a code segment, and get_thread_area read the base back"
    );
    Ok(())
}

/// [`TLS_I386_LOW`] and [`TLS_I386_HIGH`] at once, pinned to this processor
/// so that each `sched_yield` can hand it straight to the other. Answers the
/// reads through `%gs` each made.
fn check_thread_areas() -> Result<u32, &'static str> {
    /// Reads through `%gs` each program makes.
    const READS: u32 = 200;
    let cpu = crate::smp::this_cpu()
        .ok_or("the per-CPU register is not installed")?
        .logical;
    let deadline = crate::timer::now_nanos().saturating_add(30_000_000_000);
    let mut started = alloc::vec::Vec::new();
    let programs: [(&[u8], &[u8]); 2] =
        [(b"/tls-low", TLS_I386_LOW), (b"/tls-high", TLS_I386_HIGH)];
    for (name, code) in programs {
        let file = crate::syscall::image::build_with(
            ferrix_elf::Class::Elf32,
            EM_386,
            crate::syscall::image::Shape::Good,
            code,
        );
        let process =
            crate::syscall::exec::load(&file, &[name], &[], [0x7e; ferrix_ustack::RANDOM_BYTES])
                .map_err(|_| "an i386 thread-area program could not be loaded")?;
        let task = crate::syscall::process::start_on(&process, Some(cpu))
            .map_err(|_| "an i386 thread-area program could not be started")?;
        started.push((process, task));
    }
    for (process, _task) in &started {
        match process.wait_for_exit(deadline) {
            Some(HELLO_I386_STATUS) => {}
            Some(46) => {
                return Err("an i386 program read another's thread-local segment through %gs");
            }
            Some(status) => {
                println!("  i386     a thread-area program ended with {status}, not 42");
                return Err("set_thread_area or get_thread_area did not answer as Linux does");
            }
            None => return Err("an i386 thread-area program never ended"),
        }
    }
    Ok(READS)
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
