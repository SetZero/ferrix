//! Raw system calls: the only way this library reaches the kernel.
//!
//! These return the kernel's value unchanged, where `-4095..=-1` is `-errno`.
//! [`crate::errno`] turns that into C's convention of `-1` plus `errno`.

use core::arch::asm;
use core::ffi::c_int;

/// System call numbers, checked against `asm/unistd_64.h`.
pub mod nr {
    /// `read`.
    pub const READ: usize = 0;
    /// `write`.
    pub const WRITE: usize = 1;
    /// `exit_group`: ends every thread in the process.
    pub const EXIT_GROUP: usize = 231;
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
    // SAFETY: the caller vouches for the call and its arguments. `syscall`
    // clobbers only rax, rcx and r11, which are all declared.
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
