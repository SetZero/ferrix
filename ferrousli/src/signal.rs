//! `raise` and `abort`, and the stack protector's failure path.
//!
//! Only as much of signals as a single-threaded program needs to end itself.
//! `sigaction` and signal masks for programs come later.

use core::ffi::{c_int, c_ulong};

use crate::errno;
use crate::syscall::{self, nr};

/// `SIGABRT`.
pub const SIGABRT: c_int = 6;

/// `rt_sigprocmask`'s request to unblock.
const SIG_UNBLOCK: usize = 1;

/// The size of the kernel's signal set, 64 signals in 8 bytes. It is not C's
/// `sigset_t`, which is 128 bytes.
const KERNEL_SIGSET_SIZE: usize = 8;

/// The kernel's `struct sigaction` on x86-64.
#[repr(C)]
#[derive(Debug)]
struct KernelSigaction {
    handler: usize,
    flags: c_ulong,
    restorer: usize,
    mask: u64,
}

/// Sends `sig` to the calling thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn raise(sig: c_int) -> c_int {
    // SAFETY: `getpid` and `gettid` take no arguments.
    let pid = unsafe { syscall::syscall0(nr::GETPID) };
    // SAFETY: as above.
    let tid = unsafe { syscall::syscall0(nr::GETTID) };
    // SAFETY: `tgkill` reads no memory. Casting the signal sign-extends, and
    // the kernel reads the low 32 bits back.
    let ret = unsafe {
        syscall::syscall3(
            nr::TGKILL,
            pid.cast_unsigned(),
            tid.cast_unsigned(),
            sig as usize,
        )
    };
    // Zero or -1.
    errno::from_syscall(ret) as c_int
}

/// Ends the process with `SIGABRT`.
///
/// A handler for `SIGABRT` runs first. If it returns, or the signal is blocked
/// or ignored, the default action is restored and the signal is unblocked and
/// raised again, which cannot be refused.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn abort() -> ! {
    let _ = raise(SIGABRT);

    let default = KernelSigaction {
        handler: 0,
        flags: 0,
        restorer: 0,
        mask: 0,
    };
    // SAFETY: the kernel reads `default`, which outlives the call, and writes
    // nothing, since the old action's pointer is null.
    let _ = unsafe {
        syscall::syscall4(
            nr::RT_SIGACTION,
            SIGABRT as usize,
            (&raw const default).addr(),
            0,
            KERNEL_SIGSET_SIZE,
        )
    };
    let set: u64 = 1 << (SIGABRT - 1);
    // SAFETY: as above, for the signal set.
    let _ = unsafe {
        syscall::syscall4(
            nr::RT_SIGPROCMASK,
            SIG_UNBLOCK,
            (&raw const set).addr(),
            0,
            KERNEL_SIGSET_SIZE,
        )
    };
    let _ = raise(SIGABRT);

    // Nothing can refuse the default action, so this is not reached.
    syscall::exit_group(127)
}

/// Called by a function the stack protector guards, when its canary was
/// overwritten. The message is glibc's.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __stack_chk_fail() -> ! {
    const MESSAGE: &[u8] = b"*** stack smashing detected ***: terminated\n";
    // SAFETY: `MESSAGE` is a static, and the kernel only reads it.
    let _ = unsafe { syscall::syscall3(nr::WRITE, 2, MESSAGE.as_ptr().addr(), MESSAGE.len()) };
    abort()
}
