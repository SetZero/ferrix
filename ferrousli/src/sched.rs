//! `sched.h`: yielding, CPU affinity and scheduling policy, and the
//! `pthread.h` calls that set them for one thread.
//!
//! Linux schedules threads, not processes, so a pid of 0 in these calls means
//! the calling thread. The process-scoped calls, `sched_setscheduler` and its
//! relatives, pass the pid to the kernel as glibc does, which acts on that one
//! thread. (musl refuses them with `ENOSYS` because they cannot do what POSIX
//! describes; programs built for glibc expect them to work.)

use core::ffi::{c_int, c_uint, c_ulong, c_void};
use core::sync::atomic::{AtomicI32, Ordering};

use crate::pthread_attr::SchedParam;
use crate::syscall::{self, nr};
use crate::thread::{self, Thread};
use crate::time::Timespec;
use crate::{errno, pthread};

/// Makes a system call whose result is returned in C's convention.
fn call(ret: isize) -> c_int {
    errno::from_syscall(ret) as c_int
}

/// Gives the processor to another runnable thread, if there is one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_yield() -> c_int {
    // SAFETY: `sched_yield` reads no memory.
    call(unsafe { syscall::syscall0(nr::SCHED_YIELD) })
}

/// The highest priority `policy` allows.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_get_priority_max(policy: c_int) -> c_int {
    // SAFETY: the call reads no memory.
    call(unsafe { syscall::syscall2(nr::SCHED_GET_PRIORITY_MAX, policy as usize, 0) })
}

/// The lowest priority `policy` allows.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_get_priority_min(policy: c_int) -> c_int {
    // SAFETY: the call reads no memory.
    call(unsafe { syscall::syscall2(nr::SCHED_GET_PRIORITY_MIN, policy as usize, 0) })
}

/// Stores thread `pid`'s scheduling priority in `*param`.
///
/// # Safety
///
/// `param` must be valid for a write of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_getparam(pid: c_int, param: *mut SchedParam) -> c_int {
    // SAFETY: the kernel writes an `int` at the start of the caller's
    // structure.
    call(unsafe { syscall::syscall2(nr::SCHED_GETPARAM, pid as usize, param.addr()) })
}

/// Sets thread `pid`'s scheduling priority from `*param`.
///
/// # Safety
///
/// `param` must be valid for a read of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_setparam(pid: c_int, param: *const SchedParam) -> c_int {
    // SAFETY: the kernel reads an `int` at the start of the caller's
    // structure.
    call(unsafe { syscall::syscall2(nr::SCHED_SETPARAM, pid as usize, param.addr()) })
}

/// Thread `pid`'s scheduling policy.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_getscheduler(pid: c_int) -> c_int {
    // SAFETY: the call reads no memory.
    call(unsafe { syscall::syscall2(nr::SCHED_GETSCHEDULER, pid as usize, 0) })
}

/// Sets thread `pid`'s scheduling policy and priority.
///
/// # Safety
///
/// `param` must be valid for a read of a `struct sched_param`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_setscheduler(pid: c_int, policy: c_int, param: *const SchedParam) -> c_int {
    // SAFETY: the kernel reads an `int` at the start of the caller's
    // structure.
    call(unsafe {
        syscall::syscall3(nr::SCHED_SETSCHEDULER, pid as usize, policy as usize, param.addr())
    })
}

/// Stores thread `pid`'s round-robin time slice in `*ts`.
///
/// # Safety
///
/// `ts` must be valid for a write of a `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_rr_get_interval(pid: c_int, ts: *mut Timespec) -> c_int {
    // SAFETY: the kernel writes a `struct timespec` to the caller's.
    call(unsafe { syscall::syscall2(nr::SCHED_RR_GET_INTERVAL, pid as usize, ts.addr()) })
}

/// Gets thread `tid`'s affinity into the `size` bytes at `set`, zeroing
/// those past what the kernel writes. Returns 0 or an error number.
///
/// # Safety
///
/// `set` must be valid for writes of `size` bytes.
unsafe fn get_affinity(tid: c_int, size: usize, set: *mut c_ulong) -> c_int {
    // SAFETY: the kernel writes at most `size` bytes to the caller's set.
    let ret = unsafe { syscall::syscall3(nr::SCHED_GETAFFINITY, tid as usize, size, set.addr()) };
    let written = match errno::decode(ret) {
        Ok(written) => written,
        Err(error) => return error,
    };
    let bytes = set.cast::<u8>();
    let mut i = written;
    while i < size {
        // SAFETY: `i` is below `size`, for which the caller vouches.
        unsafe { bytes.wrapping_add(i).write(0) };
        i += 1;
    }
    0
}

/// Stores the CPUs thread `pid` may run on in the `size` bytes at `set`.
///
/// # Safety
///
/// `set` must be valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_getaffinity(pid: c_int, size: usize, set: *mut c_ulong) -> c_int {
    // SAFETY: the caller's contract.
    match unsafe { get_affinity(pid, size, set) } {
        0 => 0,
        error => {
            errno::set(error);
            -1
        }
    }
}

/// Lets thread `pid` run only on the CPUs in the `size` bytes at `set`.
///
/// # Safety
///
/// `set` must be valid for reads of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_setaffinity(pid: c_int, size: usize, set: *const c_ulong) -> c_int {
    // SAFETY: the kernel reads at most `size` bytes of the caller's set.
    call(unsafe { syscall::syscall3(nr::SCHED_SETAFFINITY, pid as usize, size, set.addr()) })
}

/// The CPU the calling thread is running on.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_getcpu() -> c_int {
    let mut cpu: c_uint = 0;
    // SAFETY: the kernel writes an `unsigned` to the live local, and no node
    // or cache is asked for.
    let ret = unsafe { syscall::syscall3(nr::GETCPU, (&raw mut cpu).addr(), 0, 0) };
    match errno::decode(ret) {
        Ok(_) => cpu as c_int,
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// How many CPUs the `size` bytes at `set` hold. `CPU_COUNT` calls this.
///
/// # Safety
///
/// `set` must be valid for reads of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __sched_cpucount(size: usize, set: *const c_void) -> c_int {
    let bytes = set.cast::<u8>();
    let mut count = 0;
    let mut i = 0;
    while i < size {
        // SAFETY: `i` is below `size`, for which the caller vouches.
        count += unsafe { bytes.wrapping_add(i).read() }.count_ones();
        i += 1;
    }
    count as c_int
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

/// The error number of a raw system call, or 0.
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
    // SAFETY: the caller vouches for `t`.
    unsafe {
        with_tid(t, |tid| {
            // SAFETY: the kernel writes an `int` at the start of `param`.
            let r = error_of(unsafe { syscall::syscall2(nr::SCHED_GETPARAM, tid as usize, param.addr()) });
            if r != 0 {
                return r;
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
        })
    }
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
    // SAFETY: the caller vouches for `t` and `param`.
    unsafe {
        with_tid(t, |tid| {
            // SAFETY: the kernel reads an `int` at the start of `param`.
            error_of(unsafe {
                syscall::syscall3(nr::SCHED_SETSCHEDULER, tid as usize, policy as usize, param.addr())
            })
        })
    }
}

/// Sets thread `t`'s priority, keeping its policy.
///
/// # Safety
///
/// `t` must be a live thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setschedprio(t: *mut Thread, priority: c_int) -> c_int {
    // SAFETY: the caller vouches for `t`.
    unsafe {
        with_tid(t, |tid| {
            // SAFETY: the kernel reads the live local's `int`.
            error_of(unsafe {
                syscall::syscall2(nr::SCHED_SETPARAM, tid as usize, (&raw const priority).addr())
            })
        })
    }
}

/// Stores the CPUs thread `t` may run on in the `size` bytes at `set`.
///
/// # Safety
///
/// `t` must be a live thread, and `set` valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_getaffinity_np(t: *mut Thread, size: usize, set: *mut c_ulong) -> c_int {
    // SAFETY: the caller vouches for `t` and `set`.
    unsafe { with_tid(t, |tid| unsafe { get_affinity(tid, size, set) }) }
}

/// Lets thread `t` run only on the CPUs in the `size` bytes at `set`.
///
/// # Safety
///
/// `t` must be a live thread, and `set` valid for reads of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setaffinity_np(t: *mut Thread, size: usize, set: *const c_ulong) -> c_int {
    // SAFETY: the caller vouches for `t` and `set`.
    unsafe {
        with_tid(t, |tid| {
            // SAFETY: the kernel reads at most `size` bytes of `set`.
            error_of(unsafe { syscall::syscall3(nr::SCHED_SETAFFINITY, tid as usize, size, set.addr()) })
        })
    }
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
    let id = ((-(tid as i64) - 1) as u32).wrapping_mul(8).wrapping_add(6);
    // SAFETY: the caller vouches for `clock`.
    unsafe { clock.write(id as c_int) };
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
    fn cpus_are_counted_and_the_calling_thread_runs_on_one_it_may_use() {
        let mut set = [0 as c_ulong; 16];
        let size = core::mem::size_of_val(&set);
        // SAFETY: `set` is a live local of `size` bytes.
        assert_eq!(unsafe { sched_getaffinity(0, size, set.as_mut_ptr()) }, 0);
        // SAFETY: as above.
        let count = unsafe { __sched_cpucount(size, set.as_ptr().cast()) };
        assert!(count >= 1);
        let cpu = sched_getcpu();
        assert!(cpu >= 0);
        let word = set.get(cpu as usize / 64).copied().unwrap_or(0);
        assert_ne!(word & (1 << (cpu % 64)), 0);
        assert_eq!(sched_yield(), 0);
        assert_eq!(sched_get_priority_max(1), 99);
        assert_eq!(pthread_setconcurrency(-1), errno::EINVAL);
    }
}
