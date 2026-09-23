//! `setjmp.h`: `setjmp`, `longjmp`, `sigsetjmp` and `siglongjmp`, their
//! underscore forms, and glibc's `__sigsetjmp` and `__longjmp_chk`.
//!
//! A function that returns twice cannot be written in Rust, so all of them are
//! assembly. They are left out of unit test builds, where they would take the
//! names of the host C library's functions.
//!
//! # The buffer
//!
//! `jmp_buf` in `setjmp.h` is eight words of registers, a word of flags and a
//! 128-byte `sigset_t`, 200 bytes in all. The registers are musl's order:
//! rbx, rbp, r12, r13, r14, r15, rsp, and the return address. glibc's buffer
//! has the same size and order, with an `int` saying whether the mask was
//! saved where musl has its flags word, and the mask after it. Ferrousli uses
//! glibc's meaning of that word, so the two agree field for field.
//!
//! On AArch64 the registers are 22 words: x19 to x30, a spare word, sp, and
//! d8 to d15, 312 bytes with the flag and mask. On ARMv7-A they are r4 to r11,
//! sp and lr, then d8 to d15, in a 256-byte area aligned to 8, and the buffer
//! is 392 bytes. Both are the sizes glibc's and musl's buffers have there,
//! with the flag and the mask where glibc keeps them.
//!
//! # Saving the mask
//!
//! `sigsetjmp(env, 1)` saves every register as `setjmp` does, then asks the
//! kernel for the mask and stores it, setting the flag. It never calls
//! `setjmp`, so the address it saves is its own caller's, and the second
//! return lands there directly.
//!
//! musl instead calls `setjmp` from inside `sigsetjmp`. The saved return
//! address is then inside `sigsetjmp`, whose own return address a second
//! return would find overwritten on the stack, so musl moves that address into
//! the buffer before the call and restores the mask on the way back out.
//! Saving everything in one function needs neither trick.
//!
//! Every jump restores the mask first if the flag says it was saved, before
//! any register changes, and then jumps. `longjmp` and `siglongjmp` are the
//! same function: in musl and in glibc, `longjmp` to a buffer `sigsetjmp` filled
//! restores the mask too. `setjmp` and `_setjmp` save no mask, as in musl; they
//! clear the flag. (glibc's own `setjmp` symbol saves the mask, but glibc's
//! header renames `setjmp` to `_setjmp`, which does not.)
//!
//! # Pointer mangling
//!
//! The saved rbp, rsp and return address are mangled with the pointer guard at
//! `%fs:0x30`, as glibc does: XOR with the guard, then rotate left 17 bits. A
//! program that overwrites a `jmp_buf`, through an overflow for instance, then
//! cannot choose where `longjmp` goes without knowing the guard, which comes
//! from the kernel's random bytes. musl does not mangle. Nothing a program may
//! rely on reads those words, and glibc's scheme is kept so that tools that
//! know glibc's buffer can still read them.
//!
//! On AArch64 and ARMv7-A nothing is mangled, as in musl: glibc keeps its
//! guard there in a global of the loader's, which this library's buffer need
//! not match, since no program reads the saved registers.
//!
//! `__longjmp_chk`, which fortified glibc builds call, is `longjmp` without
//! glibc's check that the jump goes up the stack.

use core::ffi::c_ulong;
use core::mem::{offset_of, size_of};

use crate::sigset::{KERNEL_SIGSET_SIZE, SIG_BLOCK, SIG_SETMASK, SigSet};
use crate::syscall::nr;

/// The registers a buffer saves: on x86-64 rbx, rbp, r12, r13, r14, r15, rsp
/// and the return address, the last two and rbp mangled.
#[cfg(target_arch = "x86_64")]
type Registers = [c_ulong; 8];
/// The registers a buffer saves: on AArch64 x19 to x30, a spare word, sp, and
/// d8 to d15.
#[cfg(target_arch = "aarch64")]
type Registers = [c_ulong; 22];
/// The registers a buffer saves: on ARMv7-A r4 to r11, sp and lr, then d8 to
/// d15, in a 256-byte area whose `u64`s align the buffer to 8.
#[cfg(target_arch = "arm")]
type Registers = [u64; 32];

/// C's `struct __jmp_buf_tag`, which `jmp_buf` and `sigjmp_buf` are arrays of
/// one of.
#[repr(C)]
#[derive(Debug)]
pub struct JmpBuf {
    /// The saved registers.
    registers: Registers,
    /// Nonzero if `mask` holds a saved signal mask.
    mask_saved: c_ulong,
    /// The saved signal mask. Only its first word is used.
    mask: SigSet,
}

#[cfg(target_arch = "x86_64")]
const _: () = assert!(size_of::<JmpBuf>() == 200);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(JmpBuf, mask_saved) == 64);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(JmpBuf, mask) == 72);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(size_of::<JmpBuf>() == 312);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(JmpBuf, mask_saved) == 176);
#[cfg(target_arch = "aarch64")]
const _: () = assert!(offset_of!(JmpBuf, mask) == 184);
#[cfg(target_arch = "arm")]
const _: () = assert!(size_of::<JmpBuf>() == 392);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(JmpBuf, mask_saved) == 256);
#[cfg(target_arch = "arm")]
const _: () = assert!(offset_of!(JmpBuf, mask) == 260);

/// Where the thread control block keeps glibc's pointer guard. See
/// `crate::thread::Thread`.
#[cfg(target_arch = "x86_64")]
const POINTER_GUARD: usize = 0x30;

// setjmp, _setjmp, sigsetjmp and __sigsetjmp on AArch64: one body.
//
// In: x0 = env, w1 = whether to save the mask (cleared first by setjmp and
// _setjmp). The system call keeps every register but x0, so x9 holds env
// across it.
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_setjmp,\"ax\",@progbits",
    ".p2align 2",
    ".globl setjmp",
    ".type setjmp,@function",
    ".globl _setjmp",
    ".type _setjmp,@function",
    "setjmp:",
    "_setjmp:",
    "mov w1, #0",
    ".globl sigsetjmp",
    ".type sigsetjmp,@function",
    ".globl __sigsetjmp",
    ".type __sigsetjmp,@function",
    "sigsetjmp:",
    "__sigsetjmp:",
    "stp x19, x20, [x0, #0]",
    "stp x21, x22, [x0, #16]",
    "stp x23, x24, [x0, #32]",
    "stp x25, x26, [x0, #48]",
    "stp x27, x28, [x0, #64]",
    "stp x29, x30, [x0, #80]",
    "mov x2, sp",
    "str x2, [x0, #104]",
    "stp d8, d9, [x0, #112]",
    "stp d10, d11, [x0, #128]",
    "stp d12, d13, [x0, #144]",
    "stp d14, d15, [x0, #160]",
    "str xzr, [x0, #{saved}]",
    "cbz w1, 1f",
    // rt_sigprocmask(SIG_BLOCK, NULL, &env->mask, 8) reads the mask.
    "mov x9, x0",
    "mov x0, #{block}",
    "mov x1, #0",
    "add x2, x9, #{mask}",
    "mov x3, #{setsize}",
    "mov x8, #{sigprocmask}",
    "svc #0",
    // It cannot fail with a writable buffer. If it did, the mask is not saved.
    "cbnz x0, 1f",
    "mov x0, #1",
    "str x0, [x9, #{saved}]",
    "1:",
    "mov x0, #0",
    "ret",
    ".size setjmp,.-setjmp",
    ".size _setjmp,.-_setjmp",
    ".size sigsetjmp,.-sigsetjmp",
    ".size __sigsetjmp,.-__sigsetjmp",
    ".popsection",
    saved = const offset_of!(JmpBuf, mask_saved),
    mask = const offset_of!(JmpBuf, mask),
    block = const SIG_BLOCK,
    setsize = const KERNEL_SIGSET_SIZE,
    sigprocmask = const nr::RT_SIGPROCMASK,
);

// longjmp, _longjmp, siglongjmp and __longjmp_chk on AArch64: one body.
//
// In: x0 = env, w1 = the value setjmp is to return, where 0 means 1, kept in
// w10 across the system call.
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_longjmp,\"ax\",@progbits",
    ".p2align 2",
    ".globl longjmp",
    ".type longjmp,@function",
    ".globl _longjmp",
    ".type _longjmp,@function",
    ".globl siglongjmp",
    ".type siglongjmp,@function",
    ".globl __longjmp_chk",
    ".type __longjmp_chk,@function",
    "longjmp:",
    "_longjmp:",
    "siglongjmp:",
    "__longjmp_chk:",
    "cmp w1, #0",
    "csinc w10, w1, wzr, ne",
    "mov x9, x0",
    "ldr x11, [x9, #{saved}]",
    "cbz x11, 1f",
    // rt_sigprocmask(SIG_SETMASK, &env->mask, NULL, 8). A signal it unblocks
    // is delivered here, on the stack being left, before any register is
    // restored.
    "mov x0, #{setmask}",
    "add x1, x9, #{mask}",
    "mov x2, #0",
    "mov x3, #{setsize}",
    "mov x8, #{sigprocmask}",
    "svc #0",
    "1:",
    "ldp x19, x20, [x9, #0]",
    "ldp x21, x22, [x9, #16]",
    "ldp x23, x24, [x9, #32]",
    "ldp x25, x26, [x9, #48]",
    "ldp x27, x28, [x9, #64]",
    "ldp x29, x30, [x9, #80]",
    "ldr x2, [x9, #104]",
    "mov sp, x2",
    "ldp d8, d9, [x9, #112]",
    "ldp d10, d11, [x9, #128]",
    "ldp d12, d13, [x9, #144]",
    "ldp d14, d15, [x9, #160]",
    "mov w0, w10",
    "br x30",
    ".size longjmp,.-longjmp",
    ".size _longjmp,.-_longjmp",
    ".size siglongjmp,.-siglongjmp",
    ".size __longjmp_chk,.-__longjmp_chk",
    ".popsection",
    saved = const offset_of!(JmpBuf, mask_saved),
    mask = const offset_of!(JmpBuf, mask),
    setmask = const SIG_SETMASK,
    setsize = const KERNEL_SIGSET_SIZE,
    sigprocmask = const nr::RT_SIGPROCMASK,
);

// setjmp, _setjmp, sigsetjmp and __sigsetjmp on ARMv7-A: one body.
//
// In: r0 = env, r1 = whether to save the mask (cleared first by setjmp and
// _setjmp). The system call needs r7, a callee-saved register already stored
// in the buffer, and keeps every register but r0, so ip holds env across it.
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_setjmp,\"ax\",%progbits",
    ".p2align 2",
    ".arm",
    ".globl setjmp",
    ".type setjmp,%function",
    ".globl _setjmp",
    ".type _setjmp,%function",
    "setjmp:",
    "_setjmp:",
    "mov r1, #0",
    ".globl sigsetjmp",
    ".type sigsetjmp,%function",
    ".globl __sigsetjmp",
    ".type __sigsetjmp,%function",
    "sigsetjmp:",
    "__sigsetjmp:",
    "mov ip, r0",
    "stmia ip!, {{r4, r5, r6, r7, r8, r9, r10, r11}}",
    "mov r2, sp",
    "stmia ip!, {{r2, lr}}",
    "vstmia ip, {{d8-d15}}",
    "mov ip, r0",
    "mov r2, #0",
    "str r2, [ip, #{saved}]",
    "cmp r1, #0",
    "beq 1f",
    // rt_sigprocmask(SIG_BLOCK, NULL, &env->mask, 8) reads the mask.
    "mov r0, #{block}",
    "mov r1, #0",
    "add r2, ip, #{mask}",
    "mov r3, #{setsize}",
    "mov r7, #{sigprocmask}",
    "svc #0",
    "ldr r7, [ip, #12]",
    // It cannot fail with a writable buffer. If it did, the mask is not saved.
    "cmp r0, #0",
    "bne 1f",
    "mov r2, #1",
    "str r2, [ip, #{saved}]",
    "1:",
    "mov r0, #0",
    "bx lr",
    ".size setjmp,.-setjmp",
    ".size _setjmp,.-_setjmp",
    ".size sigsetjmp,.-sigsetjmp",
    ".size __sigsetjmp,.-__sigsetjmp",
    ".popsection",
    saved = const offset_of!(JmpBuf, mask_saved),
    mask = const offset_of!(JmpBuf, mask),
    block = const SIG_BLOCK,
    setsize = const KERNEL_SIGSET_SIZE,
    sigprocmask = const nr::RT_SIGPROCMASK,
);

// longjmp, _longjmp, siglongjmp and __longjmp_chk on ARMv7-A: one body.
//
// In: r0 = env, r1 = the value setjmp is to return, where 0 means 1. The
// value waits in lr, which the buffer restores last, and r7, which the system
// call needs, is restored from the buffer after it.
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_longjmp,\"ax\",%progbits",
    ".p2align 2",
    ".arm",
    ".globl longjmp",
    ".type longjmp,%function",
    ".globl _longjmp",
    ".type _longjmp,%function",
    ".globl siglongjmp",
    ".type siglongjmp,%function",
    ".globl __longjmp_chk",
    ".type __longjmp_chk,%function",
    "longjmp:",
    "_longjmp:",
    "siglongjmp:",
    "__longjmp_chk:",
    "mov ip, r0",
    "movs lr, r1",
    "moveq lr, #1",
    "ldr r2, [ip, #{saved}]",
    "cmp r2, #0",
    "beq 1f",
    // rt_sigprocmask(SIG_SETMASK, &env->mask, NULL, 8). A signal it unblocks
    // is delivered here, on the stack being left, before any register is
    // restored.
    "mov r0, #{setmask}",
    "add r1, ip, #{mask}",
    "mov r2, #0",
    "mov r3, #{setsize}",
    "mov r7, #{sigprocmask}",
    "svc #0",
    "1:",
    "mov r0, lr",
    "ldmia ip!, {{r4, r5, r6, r7, r8, r9, r10, r11}}",
    "ldmia ip!, {{r2, lr}}",
    "mov sp, r2",
    "vldmia ip, {{d8-d15}}",
    "bx lr",
    ".size longjmp,.-longjmp",
    ".size _longjmp,.-_longjmp",
    ".size siglongjmp,.-siglongjmp",
    ".size __longjmp_chk,.-__longjmp_chk",
    ".popsection",
    saved = const offset_of!(JmpBuf, mask_saved),
    mask = const offset_of!(JmpBuf, mask),
    setmask = const SIG_SETMASK,
    setsize = const KERNEL_SIGSET_SIZE,
    sigprocmask = const nr::RT_SIGPROCMASK,
);

// setjmp, _setjmp, sigsetjmp and __sigsetjmp: one body.
//
// In: rdi = env, esi = whether to save the mask (not read by setjmp and
// _setjmp, which clear it first). Uses only rax, rcx, rdx, rsi, rdi, r8, r10
// and r11, which the caller does not expect kept.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_setjmp,\"ax\",@progbits",
    ".p2align 4",
    ".globl setjmp",
    ".type setjmp,@function",
    ".globl _setjmp",
    ".type _setjmp,@function",
    "setjmp:",
    "_setjmp:",
    "xor %esi, %esi",
    ".globl sigsetjmp",
    ".type sigsetjmp,@function",
    ".globl __sigsetjmp",
    ".type __sigsetjmp,@function",
    "sigsetjmp:",
    "__sigsetjmp:",
    "mov %rbx, 0(%rdi)",
    "mov %rbp, %rdx",
    "xor %fs:{guard}, %rdx",
    "rol $17, %rdx",
    "mov %rdx, 8(%rdi)",
    "mov %r12, 16(%rdi)",
    "mov %r13, 24(%rdi)",
    "mov %r14, 32(%rdi)",
    "mov %r15, 40(%rdi)",
    // The caller's stack pointer once this returns: above the return address.
    "lea 8(%rsp), %rdx",
    "xor %fs:{guard}, %rdx",
    "rol $17, %rdx",
    "mov %rdx, 48(%rdi)",
    "mov (%rsp), %rdx",
    "xor %fs:{guard}, %rdx",
    "rol $17, %rdx",
    "mov %rdx, 56(%rdi)",
    "movq $0, {saved}(%rdi)",
    "test %esi, %esi",
    "jz .Lsetjmp_done",
    // rt_sigprocmask(SIG_BLOCK, NULL, &env->mask, 8) reads the mask. The
    // kernel keeps every register but rax, rcx and r11, so r8 holds env.
    "mov %rdi, %r8",
    "mov ${block}, %edi",
    "xor %esi, %esi",
    "lea {mask}(%r8), %rdx",
    "mov ${setsize}, %r10d",
    "mov ${sigprocmask}, %eax",
    "syscall",
    // It cannot fail with a writable buffer. If it did, the mask is not saved.
    "test %rax, %rax",
    "jnz .Lsetjmp_done",
    "movq $1, {saved}(%r8)",
    ".Lsetjmp_done:",
    "xor %eax, %eax",
    "ret",
    ".size setjmp,.-setjmp",
    ".size _setjmp,.-_setjmp",
    ".size sigsetjmp,.-sigsetjmp",
    ".size __sigsetjmp,.-__sigsetjmp",
    ".popsection",
    guard = const POINTER_GUARD,
    saved = const offset_of!(JmpBuf, mask_saved),
    mask = const offset_of!(JmpBuf, mask),
    block = const SIG_BLOCK,
    setsize = const KERNEL_SIGSET_SIZE,
    sigprocmask = const nr::RT_SIGPROCMASK,
    options(att_syntax),
);

// longjmp, _longjmp, siglongjmp and __longjmp_chk: one body.
//
// In: rdi = env, esi = the value setjmp is to return, where 0 means 1.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_longjmp,\"ax\",@progbits",
    ".p2align 4",
    ".globl longjmp",
    ".type longjmp,@function",
    ".globl _longjmp",
    ".type _longjmp,@function",
    ".globl siglongjmp",
    ".type siglongjmp,@function",
    ".globl __longjmp_chk",
    ".type __longjmp_chk,@function",
    "longjmp:",
    "_longjmp:",
    "siglongjmp:",
    "__longjmp_chk:",
    "mov %esi, %r9d",
    "test %r9d, %r9d",
    "jnz .Llongjmp_value",
    "mov $1, %r9d",
    ".Llongjmp_value:",
    "cmpq $0, {saved}(%rdi)",
    "je .Llongjmp_registers",
    // rt_sigprocmask(SIG_SETMASK, &env->mask, NULL, 8). A signal it unblocks
    // is delivered here, on the stack being left, before any register is
    // restored.
    "mov %rdi, %r8",
    "mov ${setmask}, %edi",
    "lea {mask}(%r8), %rsi",
    "xor %edx, %edx",
    "mov ${setsize}, %r10d",
    "mov ${sigprocmask}, %eax",
    "syscall",
    "mov %r8, %rdi",
    ".Llongjmp_registers:",
    "mov %r9d, %eax",
    "mov 0(%rdi), %rbx",
    "mov 16(%rdi), %r12",
    "mov 24(%rdi), %r13",
    "mov 32(%rdi), %r14",
    "mov 40(%rdi), %r15",
    "mov 8(%rdi), %rdx",
    "ror $17, %rdx",
    "xor %fs:{guard}, %rdx",
    "mov %rdx, %rbp",
    "mov 48(%rdi), %rdx",
    "ror $17, %rdx",
    "xor %fs:{guard}, %rdx",
    "mov %rdx, %rsp",
    "mov 56(%rdi), %rdx",
    "ror $17, %rdx",
    "xor %fs:{guard}, %rdx",
    "jmp *%rdx",
    ".size longjmp,.-longjmp",
    ".size _longjmp,.-_longjmp",
    ".size siglongjmp,.-siglongjmp",
    ".size __longjmp_chk,.-__longjmp_chk",
    ".popsection",
    guard = const POINTER_GUARD,
    saved = const offset_of!(JmpBuf, mask_saved),
    mask = const offset_of!(JmpBuf, mask),
    setmask = const SIG_SETMASK,
    setsize = const KERNEL_SIGSET_SIZE,
    sigprocmask = const nr::RT_SIGPROCMASK,
    options(att_syntax),
);
