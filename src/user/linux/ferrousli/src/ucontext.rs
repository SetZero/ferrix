//! `ucontext.h`'s `getcontext`, `setcontext`, `swapcontext` and
//! `makecontext`, on x86-64: saving the calling thread's registers, signal
//! mask and floating-point control state in a `ucontext_t`, resuming one, and
//! preparing one to start a function on a stack of its own.
//!
//! POSIX removed them in 2008, and musl has never had them; `cargo` imports
//! all four, and the loader binds every import when a program starts, so
//! without them it does not. They are glibc's, instruction for instruction
//! where it matters: the same registers saved -- those a function call
//! preserves, the argument registers, the stack pointer and the return
//! address -- the x87 environment and `MXCSR` in `__fpregs_mem`, and
//! `makecontext`'s stack laid out as glibc lays it, with the function's
//! return going to a trampoline that resumes `uc_link`, or exits if it is
//! null.
//!
//! # Layout
//!
//! glibc's `ucontext_t` and this library's (`include/bits/x86_64/signal.h`)
//! agree up to glibc's four shadow-stack words at the end, which nothing here
//! touches. The offsets below were read from glibc 2.43's `<sys/ucontext.h>`
//! with `offsetof`, not written from memory:
//!
//! | field | offset |
//! |---|---|
//! | `uc_link` | 8 |
//! | `uc_stack.ss_sp`, `ss_size` | 16, 32 |
//! | `uc_mcontext.gregs[i]` | 40 + 8i |
//! | `uc_mcontext.fpregs` | 224 |
//! | `uc_sigmask` | 296 |
//! | `__fpregs_mem`, its `mxcsr` | 424, 448 |
//!
//! and `gregs`' indices are `REG_R8` 0, `R9` 1, `R12` 4 to `R15` 7, `RDI` 8,
//! `RSI` 9, `RBP` 10, `RBX` 11, `RDX` 12, `RCX` 14, `RSP` 15 and `RIP` 16.
//!
//! AArch64 and ARMv7-A have none of the four yet.

use core::ffi::c_int;

/// What `getcontext`, `setcontext` and `swapcontext` answer when
/// `rt_sigprocmask` refuses, from its raw result.
extern "C" fn failed(ret: isize) -> c_int {
    crate::errno::from_syscall(ret) as c_int
}

#[cfg(all(not(test), target_arch = "x86_64"))]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_ucontext,\"ax\",@progbits",
    ".p2align 4",
    // getcontext(ucp): every register a caller may find again, then the
    // signal mask.
    ".globl getcontext",
    ".type getcontext, @function",
    "getcontext:",
    "    mov qword ptr [rdi + 40], r8",
    "    mov qword ptr [rdi + 48], r9",
    "    mov qword ptr [rdi + 72], r12",
    "    mov qword ptr [rdi + 80], r13",
    "    mov qword ptr [rdi + 88], r14",
    "    mov qword ptr [rdi + 96], r15",
    "    mov qword ptr [rdi + 104], rdi",
    "    mov qword ptr [rdi + 112], rsi",
    "    mov qword ptr [rdi + 120], rbp",
    "    mov qword ptr [rdi + 128], rbx",
    "    mov qword ptr [rdi + 136], rdx",
    "    mov qword ptr [rdi + 152], rcx",
    // The caller's stack pointer and where it resumes: past and at the
    // return address.
    "    mov rcx, qword ptr [rsp]",
    "    mov qword ptr [rdi + 168], rcx",
    "    lea rcx, [rsp + 8]",
    "    mov qword ptr [rdi + 160], rcx",
    // `fpregs` points into the context itself. `fnstenv` masks the x87's
    // exceptions as it saves, so the environment is loaded straight back.
    "    lea rcx, [rdi + 424]",
    "    mov qword ptr [rdi + 224], rcx",
    "    fnstenv [rcx]",
    "    fldenv [rcx]",
    "    stmxcsr dword ptr [rdi + 448]",
    // rt_sigprocmask(SIG_BLOCK, NULL, &uc_sigmask, 8): the mask, unchanged.
    "    lea rdx, [rdi + 296]",
    "    xor esi, esi",
    "    xor edi, edi",
    "    mov r10d, 8",
    "    mov eax, 14",
    "    syscall",
    "    cmp rax, -4095",
    "    jae 1f",
    "    xor eax, eax",
    "    ret",
    "1:",
    "    mov rdi, rax",
    "    jmp {failed}",
    ".size getcontext, . - getcontext",
    //
    // setcontext(ucp): the signal mask first, while this stack is still the
    // stack, then everything else, and a return to the saved address.
    ".globl setcontext",
    ".type setcontext, @function",
    "setcontext:",
    "    push rdi",
    // rt_sigprocmask(SIG_SETMASK, &uc_sigmask, NULL, 8).
    "    lea rsi, [rdi + 296]",
    "    xor edx, edx",
    "    mov edi, 2",
    "    mov r10d, 8",
    "    mov eax, 14",
    "    syscall",
    "    pop rdx",
    "    cmp rax, -4095",
    "    jae 1f",
    "2:",
    "    mov rcx, qword ptr [rdx + 224]",
    "    fldenv [rcx]",
    "    ldmxcsr dword ptr [rdx + 448]",
    "    mov rsp, qword ptr [rdx + 160]",
    "    mov rbx, qword ptr [rdx + 128]",
    "    mov rbp, qword ptr [rdx + 120]",
    "    mov r12, qword ptr [rdx + 72]",
    "    mov r13, qword ptr [rdx + 80]",
    "    mov r14, qword ptr [rdx + 88]",
    "    mov r15, qword ptr [rdx + 96]",
    "    push qword ptr [rdx + 168]",
    "    mov rsi, qword ptr [rdx + 112]",
    "    mov rdi, qword ptr [rdx + 104]",
    "    mov rcx, qword ptr [rdx + 152]",
    "    mov r8, qword ptr [rdx + 40]",
    "    mov r9, qword ptr [rdx + 48]",
    "    mov rdx, qword ptr [rdx + 136]",
    "    xor eax, eax",
    "    ret",
    "1:",
    "    mov rdi, rax",
    "    jmp {failed}",
    ".size setcontext, . - setcontext",
    //
    // swapcontext(oucp, ucp): getcontext into oucp, resuming at this call's
    // return; one rt_sigprocmask that saves the old mask and sets the new;
    // then setcontext's second half from ucp.
    ".globl swapcontext",
    ".type swapcontext, @function",
    "swapcontext:",
    "    mov qword ptr [rdi + 40], r8",
    "    mov qword ptr [rdi + 48], r9",
    "    mov qword ptr [rdi + 72], r12",
    "    mov qword ptr [rdi + 80], r13",
    "    mov qword ptr [rdi + 88], r14",
    "    mov qword ptr [rdi + 96], r15",
    "    mov qword ptr [rdi + 104], rdi",
    "    mov qword ptr [rdi + 112], rsi",
    "    mov qword ptr [rdi + 120], rbp",
    "    mov qword ptr [rdi + 128], rbx",
    "    mov qword ptr [rdi + 136], rdx",
    "    mov qword ptr [rdi + 152], rcx",
    "    mov rcx, qword ptr [rsp]",
    "    mov qword ptr [rdi + 168], rcx",
    "    lea rcx, [rsp + 8]",
    "    mov qword ptr [rdi + 160], rcx",
    "    lea rcx, [rdi + 424]",
    "    mov qword ptr [rdi + 224], rcx",
    "    fnstenv [rcx]",
    "    fldenv [rcx]",
    "    stmxcsr dword ptr [rdi + 448]",
    // rt_sigprocmask(SIG_SETMASK, &ucp->uc_sigmask, &oucp->uc_sigmask, 8).
    "    push rsi",
    "    lea rdx, [rdi + 296]",
    "    lea rsi, [rsi + 296]",
    "    mov edi, 2",
    "    mov r10d, 8",
    "    mov eax, 14",
    "    syscall",
    "    pop rdx",
    "    cmp rax, -4095",
    "    jae 1f",
    "    jmp 2b",
    "1:",
    "    mov rdi, rax",
    "    jmp {failed}",
    ".size swapcontext, . - swapcontext",
    //
    // makecontext(ucp, func, argc, ...): glibc's layout. The stack ends at
    // ss_sp + ss_size; below it go the arguments past the sixth and a word
    // for uc_link, then the return address the function returns to, with
    // the stack pointer 8 past a 16-byte boundary as at any function's entry.
    // RBX keeps where uc_link is, for the trampoline.
    ".globl makecontext",
    ".type makecontext, @function",
    "makecontext:",
    "    mov r10, qword ptr [rdi + 16]",
    "    add r10, qword ptr [rdi + 32]",
    "    movsxd rdx, edx",
    // r11: the uc_link slot's index, one past the arguments on the stack.
    "    xor r11d, r11d",
    "    cmp rdx, 6",
    "    jle 3f",
    "    lea r11, [rdx - 6]",
    "3:",
    "    inc r11",
    "    lea rax, [r11 * 8]",
    "    sub r10, rax",
    "    and r10, -16",
    "    sub r10, 8",
    "    mov qword ptr [rdi + 168], rsi",
    "    lea rax, [r10 + r11 * 8]",
    "    mov qword ptr [rdi + 128], rax",
    "    mov qword ptr [rdi + 160], r10",
    "    lea rax, [rip + 9f]",
    "    mov qword ptr [r10], rax",
    "    mov rax, qword ptr [rdi + 8]",
    "    mov qword ptr [r10 + r11 * 8], rax",
    // The arguments: the first six into the registers the function reads
    // them from, the rest onto its stack above the return address. Of the
    // variadic ones, three came in RCX, R8 and R9, and the others are on
    // this call's stack from [rsp + 8].
    "    test rdx, rdx",
    "    jle 5f",
    "    mov qword ptr [rdi + 104], rcx",
    "    cmp rdx, 1",
    "    jle 5f",
    "    mov qword ptr [rdi + 112], r8",
    "    cmp rdx, 2",
    "    jle 5f",
    "    mov qword ptr [rdi + 136], r9",
    "    cmp rdx, 3",
    "    jle 5f",
    "    mov rax, qword ptr [rsp + 8]",
    "    mov qword ptr [rdi + 152], rax",
    "    cmp rdx, 4",
    "    jle 5f",
    "    mov rax, qword ptr [rsp + 16]",
    "    mov qword ptr [rdi + 40], rax",
    "    cmp rdx, 5",
    "    jle 5f",
    "    mov rax, qword ptr [rsp + 24]",
    "    mov qword ptr [rdi + 48], rax",
    "    mov rcx, 6",
    "4:",
    "    cmp rcx, rdx",
    "    jge 5f",
    "    mov rax, qword ptr [rsp + rcx * 8 - 16]",
    "    mov qword ptr [r10 + rcx * 8 - 40], rax",
    "    inc rcx",
    "    jmp 4b",
    "5:",
    "    ret",
    // The function returned: resume uc_link, or exit if there is none.
    "9:",
    "    mov rsp, rbx",
    "    mov rdi, qword ptr [rsp]",
    "    and rsp, -16",
    "    test rdi, rdi",
    "    je 6f",
    "    call setcontext",
    // setcontext returned, which means it failed.
    "    mov edi, eax",
    "    jmp 7f",
    "6:",
    "    xor edi, edi",
    "7:",
    "    call {exit}",
    "    hlt",
    ".size makecontext, . - makecontext",
    ".popsection",
    failed = sym failed,
    exit = sym crate::exit::exit,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_mask_is_minus_one_with_errno() {
        assert_eq!(failed(-(crate::errno::EINVAL as isize)), -1);
    }
}
