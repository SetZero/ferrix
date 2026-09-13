//! Raw system calls: the only way this library reaches the kernel.
//!
//! These return the kernel's value unchanged, where `-4095..=-1` is `-errno`.
//! [`crate::errno`] turns that into C's convention of `-1` plus `errno`.
//!
//! On x86-64 the number goes in rax and the arguments in rdi, rsi, rdx, r10,
//! r8 and r9. `syscall` itself overwrites rcx and r11.

use core::arch::asm;
use core::ffi::c_int;

/// System call numbers, checked against `asm/unistd_64.h`.
pub mod nr {
    /// `read`.
    pub const READ: usize = 0;
    /// `write`.
    pub const WRITE: usize = 1;
    /// `mmap`.
    pub const MMAP: usize = 9;
    /// `rt_sigaction`.
    pub const RT_SIGACTION: usize = 13;
    /// `rt_sigprocmask`.
    pub const RT_SIGPROCMASK: usize = 14;
    /// `getpid`.
    pub const GETPID: usize = 39;
    /// `arch_prctl`: sets the `%fs` base, among other things.
    pub const ARCH_PRCTL: usize = 158;
    /// `gettid`.
    pub const GETTID: usize = 186;
    /// `exit_group`: ends every thread in the process.
    pub const EXIT_GROUP: usize = 231;
    /// `tgkill`.
    pub const TGKILL: usize = 234;
}

/// Makes a system call with no arguments.
///
/// # Safety
///
/// The call must be sound to make.
#[inline]
pub unsafe fn syscall0(number: usize) -> isize {
    let ret: usize;
    // SAFETY: the caller vouches for the call. Every register `syscall`
    // clobbers is declared.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number => ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret.cast_signed()
}

/// Makes a system call with two arguments.
///
/// # Safety
///
/// As [`syscall3`].
#[inline]
pub unsafe fn syscall2(number: usize, a0: usize, a1: usize) -> isize {
    let ret: usize;
    // SAFETY: as in `syscall0`.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number => ret,
            in("rdi") a0,
            in("rsi") a1,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret.cast_signed()
}

/// Makes a system call with three arguments.
///
/// # Safety
///
/// The call must be sound with these arguments. The kernel checks that a
/// pointer names mapped memory, not that Rust or C code holds no reference to
/// what it will write there.
#[inline]
pub unsafe fn syscall3(number: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: usize;
    // SAFETY: as in `syscall0`.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret.cast_signed()
}

/// Makes a system call with four arguments.
///
/// # Safety
///
/// As [`syscall3`].
#[inline]
pub unsafe fn syscall4(number: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: usize;
    // SAFETY: as in `syscall0`.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret.cast_signed()
}

/// Makes a system call with six arguments.
///
/// # Safety
///
/// As [`syscall3`].
#[inline]
pub unsafe fn syscall6(
    number: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> isize {
    let ret: usize;
    // SAFETY: as in `syscall0`.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            in("r9") a5,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret.cast_signed()
}

/// Ends the process with `status`, without running anything first.
pub fn exit_group(status: c_int) -> ! {
    // SAFETY: `exit_group` reads no memory and does not return. Casting to
    // `usize` sign-extends, and the kernel reads the low 32 bits back.
    unsafe {
        asm!(
            "syscall",
            in("rax") nr::EXIT_GROUP,
            in("rdi") status as usize,
            options(noreturn, nostack),
        );
    }
}

/// Stops the program at an undefined instruction, which the kernel delivers as
/// `SIGILL`.
pub fn trap() -> ! {
    // SAFETY: `ud2` touches no state; it only faults.
    unsafe {
        asm!("ud2", options(noreturn, nostack));
    }
}
