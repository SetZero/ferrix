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

// AArch64: the kernel leaves sp at argc, 16-aligned, and a dynamic loader
// leaves its `atexit` function in x0. `__libc_start_main` takes main, argc,
// argv, init, fini, rtld_fini and stack_end in x0 to x6.
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".text",
    ".global _start",
    ".type _start, %function",
    "_start:",
    // The outermost frame: a zero frame pointer and link register end every
    // backtrace here.
    "    mov x29, #0",
    "    mov x30, #0",
    "    mov x5, x0",
    "    ldr x1, [sp]",
    "    add x2, sp, #8",
    "    mov x6, sp",
    // No `init` or `fini`, as glibc 2.34's crt1.o passes.
    "    mov x3, #0",
    "    mov x4, #0",
    "    adrp x0, :got:main",
    "    ldr x0, [x0, :got_lo12:main]",
    "    bl __libc_start_main",
    "    udf #0",
);

// ARMv7-A, in ARM state: the kernel leaves sp at argc and a dynamic loader
// leaves its `atexit` function in r0. `__libc_start_main` takes main, argc,
// argv and init in r0 to r3, and fini, rtld_fini and stack_end on the stack.
// Popping argc and pushing three words leaves sp 8-aligned, as the ABI wants
// at a call. `main` is reached through the GOT, found relative to the program
// counter, so the same object links into a position-independent program.
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    ".text",
    ".arm",
    ".global _start",
    ".type _start, %function",
    "_start:",
    "    mov fp, #0",
    "    mov lr, #0",
    "    pop {{r1}}",
    "    mov r2, sp",
    "    push {{r2}}",
    "    push {{r0}}",
    "    mov r3, #0",
    "    push {{r3}}",
    "    ldr ip, 2f",
    "1:  add ip, pc, ip",
    "    ldr r0, 3f",
    "    ldr r0, [ip, r0]",
    "    bl __libc_start_main",
    "    udf #0",
    "2:  .word _GLOBAL_OFFSET_TABLE_ - (1b + 8)",
    "3:  .word main(GOT)",
);
