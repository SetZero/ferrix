//! `sys/wait.h`: waiting for child processes to change state.
//!
//! Every call is the kernel's `wait4` or `waitid`, which every architecture
//! has.

use core::ffi::{c_int, c_uint, c_void};
use core::ptr::null_mut;

use crate::errno;
use crate::resource::Rusage;
use crate::syscall::nr;

/// Waits for child `pid`, as `waitpid` selects it, stores its status in
/// `*status` and its resource usage in `*usage`, each if not null.
///
/// # Safety
///
/// `status` must be null or valid for a write of an `int`, and `usage` null
/// or valid for a write of a `struct rusage`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wait4(
    pid: c_int,
    status: *mut c_int,
    options: c_int,
    usage: *mut Rusage,
) -> c_int {
    // SAFETY: the kernel writes `status` and `usage` when they are not null,
    // as the caller vouches.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::WAIT4,
            pid as usize,
            status.addr(),
            options as usize,
            crate::resource::kernel_usage(usage),
            0,
            0,
        )
    };
    if ret > 0 {
        // SAFETY: the kernel wrote the usage of the child it reports.
        unsafe { crate::resource::widen(usage) };
    }
    errno::from_syscall(ret) as c_int
}

/// Waits for any child, as [`wait4`] does.
///
/// # Safety
///
/// As [`wait4`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wait3(status: *mut c_int, options: c_int, usage: *mut Rusage) -> c_int {
    // SAFETY: the caller's contract is `wait4`'s.
    unsafe { wait4(-1, status, options, usage) }
}

/// Waits for child `pid`: a process id, any child for -1, or a process group
/// for 0 or less.
///
/// # Safety
///
/// `status` must be null or valid for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int {
    // SAFETY: the caller's contract is `wait4`'s, with no usage.
    unsafe { wait4(pid, status, options, null_mut()) }
}

/// Waits for any child to end.
///
/// # Safety
///
/// `status` must be null or valid for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wait(status: *mut c_int) -> c_int {
    // SAFETY: the caller's contract is `waitpid`'s.
    unsafe { waitpid(-1, status, 0) }
}

/// Waits for a child selected by `idtype` and `id`, and describes it in
/// `*info`.
///
/// `info` is a `siginfo_t`, 128 bytes (the kernel's `SI_MAX_SIZE`), whose
/// layout the signal functions define; here it is only passed through.
///
/// # Safety
///
/// `info` must be null or valid for a write of a `siginfo_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn waitid(
    idtype: c_int,
    id: c_uint,
    info: *mut c_void,
    options: c_int,
) -> c_int {
    // SAFETY: the kernel writes `info` when it is not null, as the caller
    // vouches, and no usage is asked for.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::WAITID,
            idtype as usize,
            id as usize,
            info.addr(),
            options as usize,
            0,
            0,
        )
    };
    errno::from_syscall(ret) as c_int
}
