//! `sched.h`: yielding the processor, scheduling policy and priority, the
//! affinity calls, and the namespace calls `setns` and `unshare`; and the
//! `pthread.h` calls that set policy, priority and affinity for one thread.
//!
//! Each `sched_` call is its system call. Linux schedules threads, not
//! processes, so a pid of 0 means the calling thread, and the process-scoped
//! calls, `sched_setscheduler` and its relatives, pass the pid to the kernel as
//! glibc does, which acts on that one thread. (musl refuses them with `ENOSYS`
//! because they cannot do what POSIX describes; programs built for glibc
//! expect them to work.)
//!
//! The kernel writes only as much of an affinity mask as it has processors
//! for, so `sched_getaffinity` zeroes the rest of the caller's set, as musl's
//! does. `__sched_cpucount` is what C's `CPU_COUNT` macro calls.

use core::ffi::{c_int, c_uint, c_void};
use core::sync::atomic::{AtomicI32, Ordering};

use crate::pthread_attr::SchedParam;
use crate::syscall::{self, nr};
use crate::thread::{self, Thread};
use crate::time::Timespec;
use crate::{errno, pthread};

/// Gives up the processor to another runnable thread, if there is one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_yield() -> c_int {
    // SAFETY: `sched_yield` reads no memory.
    let ret = unsafe { syscall::syscall0(nr::SCHED_YIELD) };
    errno::from_syscall(ret) as c_int
}

/// The highest priority `policy` allows.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_get_priority_max(policy: c_int) -> c_int {
    // SAFETY: the call reads no memory.
    let ret = unsafe { syscall::syscall2(nr::SCHED_GET_PRIORITY_MAX, policy as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// The lowest priority `policy` allows.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_get_priority_min(policy: c_int) -> c_int {
    // SAFETY: the call reads no memory.
    let ret = unsafe { syscall::syscall2(nr::SCHED_GET_PRIORITY_MIN, policy as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// Stores thread `pid`'s scheduling priority in `*param`.
///
/// # Safety
///
/// `param` must be valid for a write of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_getparam(pid: c_int, param: *mut SchedParam) -> c_int {
    // SAFETY: the kernel writes an `int` to the caller's structure.
    let ret = unsafe { syscall::syscall2(nr::SCHED_GETPARAM, pid as usize, param.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Sets thread `pid`'s scheduling priority from `*param`.
///
/// # Safety
///
/// `param` must be valid for a read of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_setparam(pid: c_int, param: *const SchedParam) -> c_int {
    // SAFETY: the kernel reads an `int` from the caller's structure.
    let ret = unsafe { syscall::syscall2(nr::SCHED_SETPARAM, pid as usize, param.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Thread `pid`'s scheduling policy.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_getscheduler(pid: c_int) -> c_int {
    // SAFETY: the call reads no memory.
    let ret = unsafe { syscall::syscall2(nr::SCHED_GETSCHEDULER, pid as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// Sets thread `pid`'s scheduling policy and priority.
///
/// # Safety
///
/// `param` must be valid for a read of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_setscheduler(
    pid: c_int,
    policy: c_int,
    param: *const SchedParam,
) -> c_int {
    // SAFETY: the kernel reads an `int` from the caller's structure.
    let ret = unsafe {
        syscall::syscall3(
            nr::SCHED_SETSCHEDULER,
            pid as usize,
            policy as usize,
            param.addr(),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Stores thread `pid`'s round-robin time slice in `*ts`.
///
/// # Safety
///
/// `ts` must be valid for a write of a `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_rr_get_interval(pid: c_int, ts: *mut Timespec) -> c_int {
    // SAFETY: the kernel writes a `struct timespec` to the caller's.
    let ret = unsafe { syscall::syscall2(nr::SCHED_RR_GET_INTERVAL, pid as usize, ts.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Gets thread `tid`'s affinity into the `size` bytes at `set`, zeroing those
/// past what the kernel writes. Returns 0 or an error number.
///
/// # Safety
///
/// `set` must be valid for writes of `size` bytes.
unsafe fn get_affinity(tid: c_int, size: usize, set: *mut c_void) -> c_int {
    // SAFETY: the kernel writes at most `size` bytes to `set`, which the
    // caller vouches for.
    let ret = unsafe { syscall::syscall3(nr::SCHED_GETAFFINITY, tid as usize, size, set.addr()) };
    let written = match errno::decode(ret) {
        Ok(written) => written,
        Err(error) => return error,
    };
    for offset in written..size {
        // SAFETY: the offset is below `size`, which the caller vouches for.
        unsafe { set.cast::<u8>().wrapping_add(offset).write(0) };
    }
    0
}

/// Stores the set of processors thread `pid`, or the caller if it is zero, may
/// run on in the `size` bytes at `set`, with the bytes past the kernel's mask
/// zeroed.
///
/// # Safety
///
/// `set` must be valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_getaffinity(pid: c_int, size: usize, set: *mut c_void) -> c_int {
    // SAFETY: the caller vouches for `set`.
    match unsafe { get_affinity(pid, size, set) } {
        0 => 0,
        error => {
            errno::set(error);
            -1
        }
    }
}

/// Lets thread `pid`, or the caller if it is zero, run only on the processors
/// in the `size` bytes at `set`.
///
/// # Safety
///
/// `set` must be valid for reads of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_setaffinity(pid: c_int, size: usize, set: *const c_void) -> c_int {
    // SAFETY: the kernel reads at most `size` bytes from `set`.
    let ret = unsafe { syscall::syscall3(nr::SCHED_SETAFFINITY, pid as usize, size, set.addr()) };
    errno::from_syscall(ret) as c_int
}

/// The processor the calling thread is running on.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_getcpu() -> c_int {
    let mut cpu: c_uint = 0;
    // SAFETY: the kernel writes an `unsigned` to the live local, and no node
    // or cache is asked for.
    let ret = unsafe { syscall::syscall3(nr::GETCPU, (&raw mut cpu).addr(), 0, 0) };
    match errno::decode(ret) {
        Ok(_) => c_int::try_from(cpu).unwrap_or(c_int::MAX),
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// The number of processors in the `size` bytes of the set at `set`.
///
/// # Safety
///
/// `set` must be valid for reads of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __sched_cpucount(size: usize, set: *const c_void) -> c_int {
    let mut count: u32 = 0;
    for offset in 0..size {
        // SAFETY: the offset is below `size`, which the caller vouches for.
        let byte = unsafe { set.cast::<u8>().wrapping_add(offset).read() };
        count += byte.count_ones();
    }
    c_int::try_from(count).unwrap_or(c_int::MAX)
}

/// Moves the calling thread into the namespace open as `fd`, which must be of
/// type `nstype` unless that is zero.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setns(fd: c_int, nstype: c_int) -> c_int {
    // SAFETY: `setns` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::SETNS, fd as usize, nstype as usize) };
    errno::from_syscall(ret) as c_int
}

/// Gives the calling process its own copy of the namespaces and other state
/// the `CLONE_*` bits in `flags` name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn unshare(flags: c_int) -> c_int {
    // SAFETY: `unshare` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::UNSHARE, flags as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// Runs `op` with thread `t`'s id under its kill lock, with application
/// signals blocked, or returns `ESRCH` if it has begun to exit.
///
/// # Safety
///
/// `t` must be a thread that has not been joined or ended detached.
unsafe fn with_tid(t: *mut Thread, op: impl FnOnce(c_int) -> c_int) -> c_int {
    let mask = pthread::block_app_signals();
    // SAFETY: the caller vouches for `t`.
    let state = unsafe { thread::state(t) };
    state.kill_lock.acquire();
    // SAFETY: as above.
    let tid = unsafe { thread::tid(t) }.load(Ordering::SeqCst);
    let r = if tid == 0 { errno::ESRCH } else { op(tid) };
    state.kill_lock.release();
    pthread::restore_signals(mask);
    r
}

/// The error number of a raw system call's result, or 0.
fn error_of(ret: isize) -> c_int {
    errno::decode(ret).err().unwrap_or(0)
}

/// Stores thread `t`'s policy in `*policy` and priority in `*param`.
///
/// # Safety
///
/// `t` must be a live thread, `policy` valid for a write of an `int`, and
/// `param` of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_getschedparam(
    t: *mut Thread,
    policy: *mut c_int,
    param: *mut SchedParam,
) -> c_int {
    let op = |tid: c_int| {
        // SAFETY: the kernel writes an `int` to `param`, which the caller
        // vouches for.
        let ret = unsafe { syscall::syscall2(nr::SCHED_GETPARAM, tid as usize, param.addr()) };
        if let Err(error) = errno::decode(ret) {
            return error;
        }
        // SAFETY: the call reads no memory.
        let ret = unsafe { syscall::syscall2(nr::SCHED_GETSCHEDULER, tid as usize, 0) };
        match errno::decode(ret) {
            Ok(found) => {
                // SAFETY: the caller vouches for `policy`.
                unsafe { policy.write(found as c_int) };
                0
            }
            Err(error) => error,
        }
    };
    // SAFETY: the caller vouches for `t`.
    unsafe { with_tid(t, op) }
}

/// Sets thread `t`'s policy and priority.
///
/// # Safety
///
/// `t` must be a live thread, and `param` valid for a read of a
/// `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setschedparam(
    t: *mut Thread,
    policy: c_int,
    param: *const SchedParam,
) -> c_int {
    let op = |tid: c_int| {
        // SAFETY: the kernel reads an `int` from `param`, which the caller
        // vouches for.
        error_of(unsafe {
            syscall::syscall3(
                nr::SCHED_SETSCHEDULER,
                tid as usize,
                policy as usize,
                param.addr(),
            )
        })
    };
    // SAFETY: the caller vouches for `t`.
    unsafe { with_tid(t, op) }
}

/// Sets thread `t`'s priority, keeping its policy.
///
/// # Safety
///
/// `t` must be a live thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setschedprio(t: *mut Thread, priority: c_int) -> c_int {
    let op = |tid: c_int| {
        // SAFETY: the kernel reads a `struct sched_param`'s first field, an
        // `int`, from the live local.
        error_of(unsafe {
            syscall::syscall2(
                nr::SCHED_SETPARAM,
                tid as usize,
                (&raw const priority).addr(),
            )
        })
    };
    // SAFETY: the caller vouches for `t`.
    unsafe { with_tid(t, op) }
}

/// Stores the processors thread `t` may run on in the `size` bytes at `set`.
///
/// # Safety
///
/// `t` must be a live thread, and `set` valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_getaffinity_np(
    t: *mut Thread,
    size: usize,
    set: *mut c_void,
) -> c_int {
    // SAFETY: the caller vouches for `set`.
    let op = |tid: c_int| unsafe { get_affinity(tid, size, set) };
    // SAFETY: the caller vouches for `t`.
    unsafe { with_tid(t, op) }
}

/// Lets thread `t` run only on the processors in the `size` bytes at `set`.
///
/// # Safety
///
/// `t` must be a live thread, and `set` valid for reads of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setaffinity_np(
    t: *mut Thread,
    size: usize,
    set: *const c_void,
) -> c_int {
    let op = |tid: c_int| {
        // SAFETY: the kernel reads at most `size` bytes from `set`, which the
        // caller vouches for.
        error_of(unsafe {
            syscall::syscall3(nr::SCHED_SETAFFINITY, tid as usize, size, set.addr())
        })
    };
    // SAFETY: the caller vouches for `t`.
    unsafe { with_tid(t, op) }
}

/// Stores the clock that measures thread `t`'s CPU time in `*clock`: the
/// kernel's encoding of a thread's scheduler clock, `(-tid - 1) * 8 + 6`.
///
/// # Safety
///
/// `t` must be a live thread, and `clock` valid for a write of a `clockid_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_getcpuclockid(t: *mut Thread, clock: *mut c_int) -> c_int {
    // SAFETY: the caller vouches for `t`.
    let tid = unsafe { thread::tid(t) }.load(Ordering::SeqCst);
    let id = (-tid).wrapping_sub(1).wrapping_mul(8).wrapping_add(6);
    // SAFETY: the caller vouches for `clock`.
    unsafe { clock.write(id) };
    0
}

/// The level `pthread_setconcurrency` last set.
static CONCURRENCY: AtomicI32 = AtomicI32::new(0);

/// The concurrency level, a hint that Linux threads do not need.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_getconcurrency() -> c_int {
    CONCURRENCY.load(Ordering::Relaxed)
}

/// Records a concurrency level. A negative one fails with `EINVAL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_setconcurrency(level: c_int) -> c_int {
    if level < 0 {
        return errno::EINVAL;
    }
    CONCURRENCY.store(level, Ordering::Relaxed);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_affinity_mask_is_read_zero_filled_and_counted() {
        let mut set = [0xffu8; 128];
        // SAFETY: the set is a live local of the size given.
        let ret = unsafe { sched_getaffinity(0, set.len(), set.as_mut_ptr().cast()) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let count = unsafe { __sched_cpucount(set.len(), set.as_ptr().cast()) };
        assert!(count > 0);
        let cpu = usize::try_from(sched_getcpu()).expect("a processor");
        let byte = set.get(cpu / 8).copied().unwrap_or(0);
        assert_ne!(byte & (1 << (cpu % 8)), 0);
        // SAFETY: as above.
        let ret = unsafe { sched_setaffinity(0, set.len(), set.as_ptr().cast()) };
        assert_eq!(ret, 0);
        let bits = [0b1011_0000u8, 0, 1];
        // SAFETY: as above.
        let count = unsafe { __sched_cpucount(bits.len(), bits.as_ptr().cast()) };
        assert_eq!(count, 4);
        assert_eq!(sched_yield(), 0);
        assert_eq!(unshare(0), 0);
        assert_eq!(setns(-1, 0), -1);
    }

    #[test]
    fn priorities_are_the_kernels_and_concurrency_is_only_recorded() {
        assert_eq!(sched_get_priority_max(1), 99);
        assert_eq!(sched_get_priority_min(1), 1);
        assert_eq!(sched_getscheduler(0), 0);
        assert_eq!(pthread_setconcurrency(-1), errno::EINVAL);
        assert_eq!(pthread_setconcurrency(3), 0);
        assert_eq!(pthread_getconcurrency(), 3);
    }
}
