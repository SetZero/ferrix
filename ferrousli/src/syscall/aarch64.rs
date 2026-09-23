//! AArch64's system call instruction.
//!
//! The number goes in x8 and the arguments in x0 to x5; the result comes back
//! in x0. `svc #0` preserves every other register.

use core::arch::asm;
use core::ffi::c_int;

use super::nr;

/// Makes a system call with no arguments.
///
/// # Safety
///
/// The call must be sound to make.
#[inline]
pub unsafe fn syscall0(number: usize) -> isize {
    let ret: usize;
    // SAFETY: the caller vouches for the call. `svc` clobbers only x0.
    unsafe {
        asm!(
            "svc #0",
            in("x8") number,
            lateout("x0") ret,
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
            "svc #0",
            in("x8") number,
            inlateout("x0") a0 => ret,
            in("x1") a1,
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
            "svc #0",
            in("x8") number,
            inlateout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
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
            "svc #0",
            in("x8") number,
            inlateout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
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
            "svc #0",
            in("x8") number,
            inlateout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
            in("x4") a4,
            in("x5") a5,
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
            "svc #0",
            in("x8") nr::EXIT_GROUP,
            in("x0") status as usize,
            options(noreturn, nostack),
        );
    }
}

/// Stops the program at an undefined instruction, which the kernel delivers as
/// `SIGILL`.
pub fn trap() -> ! {
    // SAFETY: `udf` touches no state; it only faults.
    unsafe {
        asm!("udf #0", options(noreturn, nostack));
    }
}
