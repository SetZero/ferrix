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
//! `__longjmp_chk`, which fortified glibc builds call, is `longjmp` without
//! glibc's check that the jump goes up the stack.

use core::ffi::c_ulong;
use core::mem::{offset_of, size_of};

use crate::sigset::{KERNEL_SIGSET_SIZE, SIG_BLOCK, SIG_SETMASK, SigSet};
use crate::syscall::nr;

/// C's `struct __jmp_buf_tag`, which `jmp_buf` and `sigjmp_buf` are arrays of
/// one of.
#[repr(C)]
#[derive(Debug)]
pub struct JmpBuf {
    /// rbx, rbp, r12, r13, r14, r15, rsp and the return address. The last two
    /// and rbp are mangled.
    registers: [c_ulong; 8],
    /// Nonzero if `mask` holds a saved signal mask.
    mask_saved: c_ulong,
    /// The saved signal mask. Only its first word is used.
    mask: SigSet,
}

const _: () = assert!(size_of::<JmpBuf>() == 200);
const _: () = assert!(offset_of!(JmpBuf, mask_saved) == 64);
const _: () = assert!(offset_of!(JmpBuf, mask) == 72);

/// Where the thread control block keeps glibc's pointer guard. See
/// `crate::thread::Thread`.
const POINTER_GUARD: usize = 0x30;

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
