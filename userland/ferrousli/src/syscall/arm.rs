//! ARMv7-A's system call instruction, under the EABI.
//!
//! The number goes in r7 and the arguments in r0 to r5; the result comes back
//! in r0. `svc #0` preserves every other register.
//!
//! A 64-bit argument takes two registers, low word first on this
//! little-endian target, and the pair must start at an even register: a call
//! whose 64-bit argument would land in r1 and r2 leaves r1 unused and takes
//! r2 and r3. Callers split and place such arguments themselves.
//!
//! This is built for ARM state, not Thumb, where r7 would be the frame pointer
//! and could not be named here.

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
    // SAFETY: the caller vouches for the call. `svc` clobbers only r0.
    unsafe {
        asm!(
            "svc #0",
            in("r7") number,
            lateout("r0") ret,
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
            in("r7") number,
            inlateout("r0") a0 => ret,
            in("r1") a1,
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
            in("r7") number,
            inlateout("r0") a0 => ret,
            in("r1") a1,
            in("r2") a2,
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
            in("r7") number,
            inlateout("r0") a0 => ret,
            in("r1") a1,
            in("r2") a2,
            in("r3") a3,
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
            in("r7") number,
            inlateout("r0") a0 => ret,
            in("r1") a1,
            in("r2") a2,
            in("r3") a3,
            in("r4") a4,
            in("r5") a5,
            options(nostack),
        );
    }
    ret.cast_signed()
}

/// Ends the process with `status`, without running anything first.
pub fn exit_group(status: c_int) -> ! {
    // SAFETY: `exit_group` reads no memory and does not return.
    unsafe {
        asm!(
            "svc #0",
            in("r7") nr::EXIT_GROUP,
            in("r0") status as usize,
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
