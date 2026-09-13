//! Sending and waiting for signals: `raise`, `kill`, `killpg`, `sigqueue`,
//! `sigtimedwait` and its relatives, `sigaltstack`; `abort`, and the stack
//! protector's failure path.
//!
//! Dispositions are in [`crate::sigaction`], sets and the mask in
//! [`crate::sigset`], and `setjmp` in `crate::setjmp`.

use core::ffi::{c_int, c_long, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::errno;
use crate::sigaction::{self, KernelSigaction};
use crate::sigset::{self, KERNEL_SIGSET_SIZE, SIG_UNBLOCK, SigSet};
use crate::syscall::{self, nr};

/// `SIGABRT`.
pub const SIGABRT: c_int = 6;

/// `SI_QUEUE`: the `si_code` of a signal `sigqueue` sent.
pub const SI_QUEUE: c_int = -1;

/// `MINSIGSTKSZ`: the smallest alternate stack `sigaltstack` accepts.
pub const MINSIGSTKSZ: usize = 2048;
/// `SS_ONSTACK`: the alternate stack is in use.
pub const SS_ONSTACK: c_int = 1;
/// `SS_DISABLE`: there is no alternate stack.
pub const SS_DISABLE: c_int = 2;

/// C's `union sigval`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union Sigval {
    /// `sival_int`.
    pub int: c_int,
    /// `sival_ptr`.
    pub ptr: *mut c_void,
}

const _: () = assert!(size_of::<Sigval>() == 8);

impl core::fmt::Debug for Sigval {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // SAFETY: every bit pattern is a valid pointer, and the union's
        // storage is a pointer wide.
        let ptr = unsafe { self.ptr };
        f.debug_struct("Sigval").field("ptr", &ptr).finish()
    }
}

/// `siginfo_t`, as far as the library fills it in. `signal.h` and the kernel's
/// `asm-generic/siginfo.h` agree on its layout: three `int`s, then a union at
/// offset 16, 128 bytes in all.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Siginfo {
    /// `si_signo`.
    pub signo: c_int,
    /// `si_errno`.
    pub errno: c_int,
    /// `si_code`.
    pub code: c_int,
    /// Padding before the union.
    pad: c_int,
    /// `si_pid`.
    pub pid: c_int,
    /// `si_uid`.
    pub uid: u32,
    /// `si_value`.
    pub value: Sigval,
    /// The rest of the union.
    rest: [u64; 12],
}

const _: () = assert!(size_of::<Siginfo>() == 128);
const _: () = assert!(offset_of!(Siginfo, pid) == 16);
const _: () = assert!(offset_of!(Siginfo, value) == 24);

/// `stack_t`, the same in `signal.h` and the kernel's `asm/signal.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Stack {
    /// `ss_sp`.
    pub sp: *mut c_void,
    /// `ss_flags`.
    pub flags: c_int,
    /// `ss_size`.
    pub size: usize,
}

const _: () = assert!(size_of::<Stack>() == 24);
const _: () = assert!(offset_of!(Stack, flags) == 8);
const _: () = assert!(offset_of!(Stack, size) == 16);

/// `struct timespec`. musl's 64-bit layout matches the kernel's
/// `struct __kernel_timespec` from `linux/time_types.h`, which is what
/// `rt_sigtimedwait` reads on x86-64.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Timespec {
    /// `tv_sec`.
    pub tv_sec: i64,
    /// `tv_nsec`.
    pub tv_nsec: c_long,
}

const _: () = assert!(size_of::<Timespec>() == 16);

/// Sends `sig` to the calling thread. A handler for it runs before this
/// returns, unless the signal is blocked.
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

/// Sends `sig` to process `pid`, or to a group of processes as `kill(2)`
/// describes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn kill(pid: c_int, sig: c_int) -> c_int {
    // SAFETY: `kill` reads no memory. Both casts sign-extend, and the kernel
    // reads the low 32 bits back.
    let ret = unsafe { syscall::syscall2(nr::KILL, pid as usize, sig as usize) };
    errno::from_syscall(ret) as c_int
}

/// Sends `sig` to process group `pgid`, or with 0 to the caller's own. A
/// negative group fails with `EINVAL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn killpg(pgid: c_int, sig: c_int) -> c_int {
    if pgid < 0 {
        errno::set(errno::EINVAL);
        return -1;
    }
    kill(-pgid, sig)
}

/// Sends `sig` to process `pid` with `value`, which the receiver finds in
/// `si_value`, and `si_code` set to `SI_QUEUE`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sigqueue(pid: c_int, sig: c_int, value: Sigval) -> c_int {
    // SAFETY: `getuid` and `getpid` take no arguments.
    let uid = unsafe { syscall::syscall0(nr::GETUID) };
    // SAFETY: as above.
    let own_pid = unsafe { syscall::syscall0(nr::GETPID) };
    let info = Siginfo {
        signo: sig,
        errno: 0,
        code: SI_QUEUE,
        pad: 0,
        // Both calls return values that fit: ids are 32 bits.
        pid: own_pid as c_int,
        uid: uid as u32,
        value,
        rest: [0; 12],
    };
    // SAFETY: the kernel reads `info`, a live local. The casts sign-extend,
    // and the kernel reads the low 32 bits back.
    let ret = unsafe {
        syscall::syscall3(
            nr::RT_SIGQUEUEINFO,
            pid as usize,
            sig as usize,
            (&raw const info).addr(),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Waits up to `timeout` for a signal in `mask`, which should be blocked, and
/// returns its number, removing it from the pending set. With `info`, stores
/// what the kernel says about it there. A null `timeout` waits for ever.
///
/// Fails with `EAGAIN` when the time runs out. A signal outside `mask` whose
/// handler runs does not end the wait, as in musl.
///
/// # Safety
///
/// `mask` must be valid for reads of a `sigset_t`, `info` null or valid for
/// writes of a `siginfo_t`, and `timeout` null or valid for reads of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigtimedwait(
    mask: *const SigSet,
    info: *mut Siginfo,
    timeout: *const Timespec,
) -> c_int {
    loop {
        // SAFETY: the caller vouches for all three pointers, of which the
        // kernel reads one word of `mask` and all of `timeout`, and writes a
        // whole `siginfo_t`.
        let ret = unsafe {
            syscall::syscall4(
                nr::RT_SIGTIMEDWAIT,
                mask.addr(),
                info.addr(),
                timeout.addr(),
                KERNEL_SIGSET_SIZE,
            )
        };
        if errno::decode(ret) != Err(errno::EINTR) {
            return errno::from_syscall(ret) as c_int;
        }
    }
}

/// As [`sigtimedwait`], with no time limit.
///
/// # Safety
///
/// As [`sigtimedwait`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigwaitinfo(mask: *const SigSet, info: *mut Siginfo) -> c_int {
    // SAFETY: the caller's promises are the same, and a null timeout is
    // allowed.
    unsafe { sigtimedwait(mask, info, core::ptr::null()) }
}

/// Waits for a signal in `mask` and stores its number in `sig`.
///
/// Returns 0, or an error number, as POSIX says and glibc does. (musl returns
/// -1 and sets `errno`.)
///
/// # Safety
///
/// `mask` must be valid for reads of a `sigset_t`, and `sig` for writes of an
/// `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigwait(mask: *const SigSet, sig: *mut c_int) -> c_int {
    loop {
        // SAFETY: the kernel reads the caller's mask and writes nothing, since
        // `info` is null.
        let ret = unsafe {
            syscall::syscall4(nr::RT_SIGTIMEDWAIT, mask.addr(), 0, 0, KERNEL_SIGSET_SIZE)
        };
        match errno::decode(ret) {
            Ok(number) => {
                // SAFETY: the caller vouches for `sig`. A signal number fits
                // in an `int`.
                unsafe { sig.write(number as c_int) };
                return 0;
            }
            Err(errno::EINTR) => {}
            Err(error) => return error,
        }
    }
}

/// Sets or reads the alternate stack that handlers with `SA_ONSTACK` run on.
///
/// As in musl, a new stack smaller than `MINSIGSTKSZ` fails with `ENOMEM`,
/// unless it disables the stack, and `SS_ONSTACK` in its flags fails with
/// `EINVAL`. The kernel checks the rest.
///
/// # Safety
///
/// `new` must be null or valid for reads of a `stack_t`, and `old` null or
/// valid for writes of one. The stack `new` names must be memory the program
/// lets handlers use.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigaltstack(new: *const Stack, old: *mut Stack) -> c_int {
    // SAFETY: the caller passes null or a readable stack.
    if let Some(stack) = unsafe { new.as_ref() } {
        if stack.flags & SS_DISABLE == 0 && stack.size < MINSIGSTKSZ {
            errno::set(errno::ENOMEM);
            return -1;
        }
        if stack.flags & SS_ONSTACK != 0 {
            errno::set(errno::EINVAL);
            return -1;
        }
    }
    // SAFETY: the kernel reads `new` and writes `old`, for which the caller
    // vouches.
    let ret = unsafe { syscall::syscall2(nr::SIGALTSTACK, new.addr(), old.addr()) };
    errno::from_syscall(ret) as c_int
}

/// `SIGRTMIN`: 35, since the library reserves 32 to 34.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __libc_current_sigrtmin() -> c_int {
    sigset::SIGRTMIN
}

/// `SIGRTMAX`: 64.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __libc_current_sigrtmax() -> c_int {
    sigset::SIGRTMAX
}

/// Ends the process with `SIGABRT`.
///
/// A handler for `SIGABRT` runs first. If it returns, or the signal is blocked
/// or ignored, the default action is restored and the signal is unblocked and
/// raised again, which cannot be refused.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn abort() -> ! {
    let _ = raise(SIGABRT);

    // SAFETY: the default action runs no code.
    let _ = unsafe { sigaction::kernel_sigaction(SIGABRT, Some(&KernelSigaction::DEFAULT), None) };
    let set = SigSet::from_word(sigset::bit(SIGABRT).unwrap_or(0));
    // SAFETY: `set` is a live local, and there is no old mask to write.
    let _ = unsafe { sigset::pthread_sigmask(SIG_UNBLOCK, &raw const set, null_mut()) };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn errno() -> c_int {
        // SAFETY: the pointer is this thread's `errno`.
        unsafe { errno::__errno_location().read() }
    }

    #[test]
    fn a_negative_process_group_is_refused() {
        errno::set(0);
        assert_eq!(killpg(-1, 0), -1);
        assert_eq!(errno(), errno::EINVAL);
    }

    #[test]
    fn sigaltstack_checks_size_and_flags_before_the_kernel() {
        let mut memory = [0_u8; 64];
        let small = Stack {
            sp: memory.as_mut_ptr().cast(),
            flags: 0,
            size: MINSIGSTKSZ - 1,
        };
        errno::set(0);
        // SAFETY: `small` is a live local, and is refused before the kernel
        // sees it.
        assert_eq!(unsafe { sigaltstack(&raw const small, null_mut()) }, -1);
        assert_eq!(errno(), errno::ENOMEM);

        let on_stack = Stack {
            flags: -1,
            size: MINSIGSTKSZ,
            ..small
        };
        errno::set(0);
        // SAFETY: as above.
        assert_eq!(unsafe { sigaltstack(&raw const on_stack, null_mut()) }, -1);
        assert_eq!(errno(), errno::EINVAL);
    }

    #[test]
    fn the_real_time_range_starts_after_the_reserved_signals() {
        assert_eq!(__libc_current_sigrtmin(), 35);
        assert_eq!(__libc_current_sigrtmax(), 64);
    }
}
