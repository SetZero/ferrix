//! `crt1.o`: `_start`, the first instructions of every program.
//!
//! The kernel enters a static program here with nothing set up. The stack
//! pointer is at `argc`, followed by `argv`, a null, the environment, a null
//! and the auxiliary vector. `_start` passes that to `__libc_start_main` with
//! the arguments glibc's `crt1.o` passes, in the same order. A program that
//! carries glibc's own `crt1.o` can then start against this library too.

#![no_std]

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".text",
    ".global _start",
    ".type _start, @function",
    "_start:",
    // The outermost frame. A zero frame pointer ends every backtrace here.
    "    xor ebp, ebp",
    // A dynamic loader leaves a function for `atexit` in rdx; the kernel
    // leaves zero. It is the sixth argument.
    "    mov r9, rdx",
    // argc, the second argument, and argv, the third.
    "    pop rsi",
    "    mov rdx, rsp",
    // The ABI wants rsp a multiple of 16 at every call, and the kernel does
    // not promise one. Two pushes keep the alignment: padding, then the
    // seventh argument, `stack_end`, which is passed on the stack.
    "    and rsp, -16",
    "    push rax",
    "    push rsp",
    // No `init` or `fini`: since glibc 2.34 the library runs those arrays
    // itself, and `crt1.o` passes null.
    "    xor r8d, r8d",
    "    xor ecx, ecx",
    "    mov rdi, qword ptr [rip + main@GOTPCREL]",
    "    call qword ptr [rip + __libc_start_main@GOTPCREL]",
    "    hlt",
);
