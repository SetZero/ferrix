//! `pthread.h`'s threads: creating, joining, detaching and ending them; the
//! list of running threads; sending them signals; and naming them.
//!
//! The design is musl's (MIT), `src/thread/pthread_create.c` and its
//! neighbours.
//!
//! # A thread's memory
//!
//! Each thread gets one private anonymous mapping:
//!
//! ```text
//!  map_base                                                 map_base + map_size
//!  │ guard (PROT_NONE) │ stack ◄── grows down │ TLS │ control block │ keys │
//! ```
//!
//! The TLS block and control block are placed by [`thread::new_control_block`],
//! exactly as the main thread's are. The keys are the thread's
//! `PTHREAD_KEYS_MAX` thread-specific values. A program that supplies its own
//! stack gets a mapping without a guard or stack, holding only the rest.
//!
//! # Ending, and who frees the memory
//!
//! A joinable thread's memory is freed by whoever joins it. A detached thread
//! frees its own, in [`arch::unmap_self`], which unmaps and exits without
//! touching the stack it has just unmapped.
//!
//! A joiner must not unmap a thread that is still running on its stack, even
//! though that thread has already reported that it exited. Every thread is
//! created with `CLONE_CHILD_CLEARTID` pointing at [`THREAD_LIST_LOCK`], and
//! exits holding that lock, so the lock is released by the kernel, only once
//! the thread is truly gone. A joiner waits for the lock to be free before
//! unmapping. The main thread gives the kernel the same address at startup.
//!
//! # Signals across creation
//!
//! Application signals are blocked while the thread list lock is held, so
//! that a handler on the same thread cannot deadlock on it, and the new thread
//! starts with them blocked, until it has installed the mask its creator had.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::{null, null_mut, with_exposed_provenance_mut, without_provenance_mut};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicUsize, Ordering};

use crate::pthread_attr::{self, Attr};
use crate::signal::Sigval;
use crate::sigset::{self, KERNEL_SIGSET_SIZE, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK};
use crate::syscall::{self, nr};
use crate::thread::{self, Thread};
use crate::time::{CLOCK_REALTIME, Timespec};
use crate::{arch, cancel, errno, futex, key};

/// The thread has exited, and its memory may be freed.
pub const DT_EXITED: c_int = 0;
/// The thread is exiting.
pub const DT_EXITING: c_int = 1;
/// The thread is running and may be joined.
pub const DT_JOINABLE: c_int = 2;
/// The thread is running and will free its own memory.
pub const DT_DETACHED: c_int = 3;

/// The signal that carries a cancellation request. Reserved by the library.
pub const SIGCANCEL: c_int = 33;
/// The signal that runs `set*id` on every thread. Reserved by the library.
pub const SIGSYNCCALL: c_int = 34;

/// Every signal but the library's reserved ones, as a kernel set: musl's
/// application mask.
const APP_SIGNALS: u64 = 0xffff_fffc_7fff_ffff;
/// Every signal.
const ALL_SIGNALS: u64 = !0;
/// `SIGCANCEL` and `SIGSYNCCALL`.
const INTERNAL_SIGNALS: u64 = (1 << (SIGCANCEL - 1)) | (1 << (SIGSYNCCALL - 1));

/// `clone`'s flags for a thread, from `linux/sched.h`: share the address
/// space, file system information, descriptors and signal handlers; be a
/// thread of this process; share System V semaphore undo; take a thread
/// pointer; report the id to the creator; and clear the id at exit.
const CLONE_FLAGS: usize = 0x0000_0100 // CLONE_VM
    | 0x0000_0200 // CLONE_FS
    | 0x0000_0400 // CLONE_FILES
    | 0x0000_0800 // CLONE_SIGHAND
    | 0x0001_0000 // CLONE_THREAD
    | 0x0004_0000 // CLONE_SYSVSEM
    | 0x0008_0000 // CLONE_SETTLS
    | 0x0010_0000 // CLONE_PARENT_SETTID
    | 0x0020_0000; // CLONE_CHILD_CLEARTID

/// `PROT_NONE`, from `asm-generic/mman-common.h`.
const PROT_NONE: usize = 0;
/// `PROT_READ | PROT_WRITE`.
const PROT_READ_WRITE: usize = 0x1 | 0x2;
/// `MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK`, from `linux/mman.h` and
/// `asm-generic/mman-common.h`. `MAP_STACK` asks for memory suited to a
/// stack, which keeps transparent huge pages off it.
const MAP_THREAD: usize = 0x02 | 0x20 | 0x2_0000;

/// `PR_SET_NAME`, from `linux/prctl.h`.
const PR_SET_NAME: usize = 15;
/// `PR_GET_NAME`, from `linux/prctl.h`.
const PR_GET_NAME: usize = 16;
/// `O_RDONLY | O_CLOEXEC`, from `asm-generic/fcntl.h`.
const O_RDONLY_CLOEXEC: usize = 0o2_000_000;
/// `O_WRONLY | O_CLOEXEC`, from `asm-generic/fcntl.h`.
const O_WRONLY_CLOEXEC: usize = 0o2_000_001;
/// `AT_FDCWD`, from `linux/fcntl.h`.
const AT_FDCWD: c_int = -100;
/// The longest thread name, without its NUL: `TASK_COMM_LEN` less one.
const NAME_MAX: usize = 15;

/// `SI_QUEUE`, from `asm-generic/siginfo.h`.
const SI_QUEUE: c_int = -1;

/// The bytes of thread-specific values at the top of each thread's mapping.
const TSD_SIZE: usize = key::KEYS_MAX * size_of::<usize>();

/// Held while the list of threads changes, by the id of the thread holding
/// it. The kernel clears it when a thread that exited holding it is gone.
pub static THREAD_LIST_LOCK: AtomicI32 = AtomicI32::new(0);
/// How many more times the holder has taken [`THREAD_LIST_LOCK`].
static LIST_LOCK_COUNT: AtomicI32 = AtomicI32::new(0);
/// Threads sleeping on [`THREAD_LIST_LOCK`].
static LIST_LOCK_WAITERS: AtomicI32 = AtomicI32::new(0);
/// How many threads there are besides the first.
pub static THREADS_MINUS_1: AtomicUsize = AtomicUsize::new(0);
/// Whether `pthread_create` has been called.
static THREADED: AtomicBool = AtomicBool::new(false);

/// glibc's `__libc_single_threaded`: 1 until the program first creates a
/// thread, and 0 from then on. Compiled code may read it to skip locking. It
/// is never set back to 1, which is always safe.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static __libc_single_threaded: AtomicU8 = AtomicU8::new(1);

/// Changes the calling thread's signal mask with `how` and `set`, and returns
/// the old mask. The kernel's call is made directly, since the mask may hold
/// the reserved signals.
fn sigprocmask(how: c_int, set: u64) -> u64 {
    let mut old: u64 = 0;
    // SAFETY: the kernel reads one word of `set` and writes one of `old`, both
    // live locals.
    let _ = unsafe {
        syscall::syscall4(
            nr::RT_SIGPROCMASK,
            how as usize,
            (&raw const set).addr(),
            (&raw mut old).addr(),
            KERNEL_SIGSET_SIZE,
        )
    };
    old
}

/// Blocks every signal but the library's reserved ones, and returns the old
/// mask.
pub fn block_app_signals() -> u64 {
    sigprocmask(SIG_BLOCK, APP_SIGNALS)
}

/// Blocks every signal, and returns the old mask.
pub fn block_all_signals() -> u64 {
    sigprocmask(SIG_BLOCK, ALL_SIGNALS)
}

/// Restores a mask one of the blocking functions returned.
pub fn restore_signals(mask: u64) {
    let _ = sigprocmask(SIG_SETMASK, mask);
}

/// Takes the thread list lock. The holder may take it again.
///
/// Application signals must be blocked, so that a handler cannot try to take
/// it on a thread that holds it.
pub fn list_lock() {
    let tid = thread::my_tid();
    if tid != 0 && THREAD_LIST_LOCK.load(Ordering::SeqCst) == tid {
        let _ = LIST_LOCK_COUNT.fetch_add(1, Ordering::SeqCst);
        return;
    }
    loop {
        let held = futex::cas(&THREAD_LIST_LOCK, 0, tid);
        if held == 0 {
            return;
        }
        // Not private: the kernel's wake at a thread's exit is not.
        futex::wait_counted(&THREAD_LIST_LOCK, Some(&LIST_LOCK_WAITERS), held, false);
    }
}

/// Releases the thread list lock once for each time it was taken.
pub fn list_unlock() {
    if LIST_LOCK_COUNT.load(Ordering::SeqCst) > 0 {
        let _ = LIST_LOCK_COUNT.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    THREAD_LIST_LOCK.store(0, Ordering::SeqCst);
    if LIST_LOCK_WAITERS.load(Ordering::SeqCst) != 0 {
        futex::wake(&raw const THREAD_LIST_LOCK, 1, false);
    }
}

/// Waits until nobody holds the thread list lock, which, after a thread
/// has reported that it exited, means it is gone.
fn list_sync() {
    let held = THREAD_LIST_LOCK.load(Ordering::SeqCst);
    if held == 0 {
        return;
    }
    futex::wait_counted(&THREAD_LIST_LOCK, Some(&LIST_LOCK_WAITERS), held, false);
    if LIST_LOCK_WAITERS.load(Ordering::SeqCst) != 0 {
        futex::wake(&raw const THREAD_LIST_LOCK, 1, false);
    }
}

/// Calls `visit` with every thread's control block, the calling thread's
/// first. The thread list lock must be held.
pub fn for_each_thread(mut visit: impl FnMut(*mut Thread)) {
    let me = thread::current();
    let mut t = me;
    loop {
        visit(t);
        // SAFETY: a thread on the list stays mapped while the lock is held.
        t = unsafe { thread::state(t) }.next.load(Ordering::SeqCst);
        if t == me {
            return;
        }
    }
}

/// What a new thread runs.
#[derive(Debug, Clone, Copy)]
pub enum Routine {
    /// A POSIX start routine, returning a pointer.
    Posix(unsafe extern "C" fn(*mut c_void) -> *mut c_void),
    /// A C11 start routine, returning an `int`.
    C11(unsafe extern "C" fn(*mut c_void) -> c_int),
}

/// What a new thread finds at the top of its stack.
#[repr(C)]
#[derive(Debug)]
struct StartArgs {
    routine: Routine,
    arg: *mut c_void,
    /// Zero, or the handshake for explicit scheduling: 1 until the thread
    /// waits, 2 while it waits, then 0 to run or 3 to exit at once.
    control: AtomicI32,
    /// The signal mask to run with.
    mask: u64,
}

/// The kernel's `struct sched_param`, from `linux/sched/types.h`.
#[repr(C)]
#[derive(Debug)]
struct KernelSchedParam {
    priority: c_int,
}

/// Maps `size` bytes for a thread, the first `guard` of them inaccessible.
/// Returns the address, or `EAGAIN`.
fn map_thread(size: usize, guard: usize) -> Result<usize, c_int> {
    let prot = if guard > 0 {
        PROT_NONE
    } else {
        PROT_READ_WRITE
    };
    // SAFETY: a new anonymous private mapping aliases nothing.
    let ret = unsafe { syscall::mmap(0, size, prot, MAP_THREAD, usize::MAX, 0) };
    let map = errno::decode(ret).map_err(|_| errno::EAGAIN)?;
    if guard > 0 {
        // SAFETY: the range is inside the mapping just made.
        let ret =
            unsafe { syscall::syscall3(nr::MPROTECT, map + guard, size - guard, PROT_READ_WRITE) };
        if ret != 0 {
            unmap(map, size);
            return Err(errno::EAGAIN);
        }
    }
    Ok(map)
}

/// Gives a thread's mapping back.
fn unmap(base: usize, len: usize) {
    // SAFETY: only mappings this module made are passed, once nothing uses
    // them.
    let _ = unsafe { syscall::syscall2(nr::MUNMAP, base, len) };
}

/// Creates a thread running `routine(arg)` with `attr`, or the defaults if it
/// is `None`, and returns its control block or an error number.
///
/// # Safety
///
/// `routine` must be sound to run with `arg` on another thread, and a stack
/// `attr` names must be memory the thread may use.
pub unsafe fn create(
    attr: Option<&Attr>,
    routine: Routine,
    arg: *mut c_void,
) -> Result<*mut Thread, c_int> {
    if !THREADED.swap(true, Ordering::SeqCst) {
        // A program started from one with a mask holding the reserved
        // signals would never see a cancellation.
        let _ = sigprocmask(SIG_UNBLOCK, INTERNAL_SIGNALS);
        __libc_single_threaded.store(0, Ordering::SeqCst);
    }
    let attr = attr.copied().unwrap_or_else(pthread_attr::defaults);

    let page = crate::sysconf::page_size();
    let round = |n: usize| n.checked_next_multiple_of(page).ok_or(errno::EAGAIN);
    let tls_len = thread::tls_reservation().ok_or(errno::EAGAIN)?;
    let need = tls_len.checked_add(TSD_SIZE).ok_or(errno::EAGAIN)?;
    let user_stack = attr.stackaddr != 0;
    let (guard, size) = if user_stack {
        (0, round(need)?)
    } else {
        let guard = round(attr.guardsize)?;
        let body = round(attr.stacksize.checked_add(need).ok_or(errno::EAGAIN)?)?;
        (guard, guard.checked_add(body).ok_or(errno::EAGAIN)?)
    };
    let map = map_thread(size, guard)?;
    let tsd = map + size - TSD_SIZE;
    let region = tsd - tls_len;

    // SAFETY: the region is fresh zeroed memory in the mapping, and the
    // calling thread has a control block.
    let Some(new) =
        (unsafe { thread::new_control_block(with_exposed_provenance_mut(region), tls_len) })
    else {
        unmap(map, size);
        return Err(errno::EAGAIN);
    };
    let (stack_top, stack_limit) = if user_stack {
        (
            attr.stackaddr & !15,
            attr.stackaddr.wrapping_sub(attr.stacksize),
        )
    } else {
        (region, map + guard)
    };
    // SAFETY: the control block was just built in the mapping.
    let state = unsafe { thread::state(new) };
    state.map_base.store(map, Ordering::SeqCst);
    state.map_size.store(size, Ordering::SeqCst);
    state.stack.store(stack_top, Ordering::SeqCst);
    state
        .stack_size
        .store(stack_top.wrapping_sub(stack_limit), Ordering::SeqCst);
    state.guard_size.store(guard, Ordering::SeqCst);
    state
        .tsd
        .store(with_exposed_provenance_mut(tsd), Ordering::SeqCst);
    state.detach_state.store(
        if attr.detach != 0 {
            DT_DETACHED
        } else {
            DT_JOINABLE
        },
        Ordering::SeqCst,
    );

    let sp = (stack_top & !7) - size_of::<StartArgs>();
    let args = with_exposed_provenance_mut::<StartArgs>(sp);
    let explicit = attr.inherit == pthread_attr::EXPLICIT_SCHED;
    let mask = block_app_signals();
    // SAFETY: `sp` is on the new thread's stack, which nothing uses yet.
    unsafe {
        args.write(StartArgs {
            routine,
            arg,
            control: AtomicI32::new(c_int::from(explicit)),
            mask: mask & !(1 << (SIGCANCEL - 1)),
        });
    }

    // SAFETY: the control block was just built.
    let parent_tid = unsafe { thread::tid(new) }.as_ptr();
    list_lock();
    let _ = THREADS_MINUS_1.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the new thread's stack, control block and arguments are set up,
    // the kernel writes its id into its control block, and it clears the
    // thread list lock, a static, when it ends.
    let ret = unsafe {
        arch::clone(
            start,
            sp,
            CLONE_FLAGS,
            args.cast(),
            parent_tid,
            arch::clone_tls(thread::pointer_of(new)),
            THREAD_LIST_LOCK.as_ptr(),
        )
    };
    let mut result = if ret < 0 { Err(errno::EAGAIN) } else { Ok(()) };
    if ret >= 0 && explicit {
        let param = KernelSchedParam {
            priority: attr.priority,
        };
        // SAFETY: the kernel reads `param`, a live local.
        let set = unsafe {
            syscall::syscall3(
                nr::SCHED_SETSCHEDULER,
                ret as usize,
                attr.policy as usize,
                (&raw const param).addr(),
            )
        };
        // SAFETY: the arguments stay on the new thread's stack, which is not
        // freed while the thread list lock is held.
        let control = unsafe { &(*args).control };
        let failed = errno::decode(set).err();
        if control.swap(if failed.is_some() { 3 } else { 0 }, Ordering::SeqCst) == 2 {
            futex::wake(control, 1, true);
        }
        if let Some(error) = failed {
            // The thread exits at once; the kernel clears `control` then.
            futex::wait_counted(control, None, 3, false);
            result = Err(error);
        }
    }
    if result.is_ok() {
        let me = thread::current();
        // SAFETY: both control blocks are live, and the lock is held.
        let mine = unsafe { thread::state(me) };
        // SAFETY: as above.
        let theirs = unsafe { thread::state(new) };
        let next = mine.next.load(Ordering::SeqCst);
        theirs.next.store(next, Ordering::SeqCst);
        theirs.prev.store(me, Ordering::SeqCst);
        // SAFETY: as above.
        unsafe { thread::state(next) }
            .prev
            .store(new, Ordering::SeqCst);
        mine.next.store(new, Ordering::SeqCst);
    } else {
        let _ = THREADS_MINUS_1.fetch_sub(1, Ordering::SeqCst);
    }
    list_unlock();
    restore_signals(mask);
    match result {
        Ok(()) => Ok(new),
        Err(error) => {
            unmap(map, size);
            Err(error)
        }
    }
}

/// Where a new thread begins, called by the clone trampoline with its
/// [`StartArgs`].
///
/// # Safety
///
/// `p` must be the arguments [`create`] wrote.
unsafe extern "C" fn start(p: *mut c_void) -> c_int {
    let args = p.cast::<StartArgs>();
    // SAFETY: the creator wrote the arguments at the top of this stack.
    let control = unsafe { &(*args).control };
    if control.load(Ordering::SeqCst) != 0 {
        if futex::cas(control, 1, 2) == 1 {
            futex::wait_counted(control, None, 2, true);
        }
        if control.load(Ordering::SeqCst) != 0 {
            // Scheduling failed: exit, telling the creator through `control`.
            // SAFETY: `control` outlives this thread; the creator waits.
            let _ = unsafe { syscall::syscall2(nr::SET_TID_ADDRESS, control.as_ptr().addr(), 0) };
            loop {
                // SAFETY: `exit` ends only this thread.
                let _ = unsafe { syscall::syscall2(nr::EXIT, 0, 0) };
            }
        }
    }
    // SAFETY: as above.
    let args = unsafe { &*args };
    let (routine, arg, mask) = (args.routine, args.arg, args.mask);
    restore_signals(mask);
    let result = match routine {
        // SAFETY: `pthread_create`'s caller vouched for the routine.
        Routine::Posix(func) => unsafe { func(arg) },
        Routine::C11(func) => {
            // SAFETY: as above.
            let status = unsafe { func(arg) };
            without_provenance_mut(status as isize as usize)
        }
    };
    pthread_exit(result)
}

/// Creates a thread running `entry(arg)` with the attributes `*attr`, or the
/// defaults if `attr` is null, and stores its id in `*res`.
///
/// Fails with `EAGAIN` if there is no memory or no thread for it, and with
/// `EINVAL` for a null routine.
///
/// # Safety
///
/// `res` must be valid for a write of a `pthread_t`, `attr` null or an
/// initialised `pthread_attr_t`, and `entry` sound to run with `arg` on
/// another thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_create(
    res: *mut *mut Thread,
    attr: *const Attr,
    entry: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
    arg: *mut c_void,
) -> c_int {
    let Some(entry) = entry else {
        return errno::EINVAL;
    };
    // SAFETY: the caller passes null or an initialised attribute object.
    let attr = unsafe { attr.as_ref() };
    // SAFETY: the caller vouches for the routine and any stack.
    match unsafe { create(attr, Routine::Posix(entry), arg) } {
        Ok(new) => {
            // SAFETY: the caller vouches for `res`.
            unsafe { res.write(new) };
            0
        }
        Err(error) => error,
    }
}

/// Ends the calling thread with `result`, which a joiner receives.
///
/// Cleanup handlers run, innermost first, then thread-specific destructors.
/// Robust mutexes the thread holds are marked as their owner having died. If
/// this is the last thread, the process exits with status 0, running the exit
/// handlers, as if `main` had returned 0.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_exit(result: *mut c_void) -> ! {
    let me = thread::current();
    let state = thread::me();
    state
        .cancel_disable
        .store(cancel::DISABLE, Ordering::SeqCst);
    state.cancel_async.store(0, Ordering::SeqCst);
    state.result.store(result, Ordering::SeqCst);

    cancel::run_cleanup_handlers();
    key::run_destructors();

    let mask = block_app_signals();
    // A concurrent `pthread_detach` competes with this; whichever loses frees
    // the memory.
    let detach = futex::cas(&state.detach_state, DT_JOINABLE, DT_EXITING);

    state.kill_lock.acquire();
    list_lock();
    if state.next.load(Ordering::SeqCst) == me {
        // The last thread: the process ends, as `exit(0)` ends it.
        list_unlock();
        state.kill_lock.release();
        state.detach_state.store(detach, Ordering::SeqCst);
        restore_signals(mask);
        crate::exit::exit(0);
    }

    // The id may be reused once the thread is gone. With it cleared under the
    // kill lock, nothing signals a stranger.
    // SAFETY: the calling thread's control block is live.
    unsafe { thread::tid(me) }.store(0, Ordering::SeqCst);
    state.kill_lock.release();

    crate::mutex::abandon_robust_list(state);

    let _ = THREADS_MINUS_1.fetch_sub(1, Ordering::SeqCst);
    let prev = state.prev.load(Ordering::SeqCst);
    let next = state.next.load(Ordering::SeqCst);
    // SAFETY: the neighbours are live threads, and the lock is held.
    unsafe { thread::state(next) }
        .prev
        .store(prev, Ordering::SeqCst);
    // SAFETY: as above.
    unsafe { thread::state(prev) }
        .next
        .store(next, Ordering::SeqCst);
    state.prev.store(me, Ordering::SeqCst);
    state.next.store(me, Ordering::SeqCst);

    let base = state.map_base.load(Ordering::SeqCst);
    if detach == DT_DETACHED && base != 0 {
        // Nothing may run on the stack once it is gone, not even a handler.
        let _ = block_all_signals();
        if state.robust_off.load(Ordering::SeqCst) != 0 {
            // The list is about to be unmapped, and was released above.
            // SAFETY: a null list only tells the kernel there is none.
            let _ = unsafe { syscall::syscall2(nr::SET_ROBUST_LIST, 0, 3 * size_of::<usize>()) };
        }
        // SAFETY: every signal is blocked, and nothing uses the mapping once
        // this thread ends; the thread list lock the kernel will clear is a
        // static.
        unsafe { arch::unmap_self(base, state.map_size.load(Ordering::SeqCst)) }
    }

    state.detach_state.store(DT_EXITED, Ordering::SeqCst);
    futex::wake(&raw const state.detach_state, 1, true);
    loop {
        // SAFETY: `exit` ends only this thread, which holds the thread list
        // lock until the kernel clears it.
        let _ = unsafe { syscall::syscall2(nr::EXIT, 0, 0) };
    }
}

/// Waits for thread `t` to end, until the absolute time `at` on
/// `CLOCK_REALTIME` if it is not null, and frees it.
///
/// # Safety
///
/// `t` must be a thread that has not been joined, and `res` null or valid for
/// a write of a pointer, and `at` null or valid for a read of a
/// `struct timespec`.
unsafe fn timed_join(t: *mut Thread, res: *mut *mut c_void, at: *const Timespec) -> c_int {
    if t == thread::current() {
        return errno::EDEADLK;
    }
    cancel::testcancel();
    // Cancellable only if enabled: the masked state must not report
    // `ECANCELED` from here.
    let old = cancel::set_state(cancel::DISABLE);
    if old == cancel::ENABLE {
        let _ = cancel::set_state(old);
    }
    // SAFETY: the caller vouches for `t`, which is not freed until joined.
    let state = unsafe { thread::state(t) };
    let mut r = 0;
    loop {
        let detach = state.detach_state.load(Ordering::SeqCst);
        if detach == DT_EXITED || r == errno::ETIMEDOUT || r == errno::EINVAL {
            break;
        }
        if detach >= DT_DETACHED {
            r = errno::EINVAL;
            break;
        }
        // SAFETY: the caller vouches for `at`.
        r = unsafe { futex::timedwait_cp(&state.detach_state, detach, CLOCK_REALTIME, at, true) };
    }
    let _ = cancel::set_state(old);
    if r == errno::ETIMEDOUT || r == errno::EINVAL {
        return r;
    }
    list_sync();
    if !res.is_null() {
        // SAFETY: the caller vouches for `res`.
        unsafe { res.write(state.result.load(Ordering::SeqCst)) };
    }
    let base = state.map_base.load(Ordering::SeqCst);
    if base != 0 {
        unmap(base, state.map_size.load(Ordering::SeqCst));
    }
    0
}

/// Waits for thread `t` to end, stores what it returned in `*res` if `res`
/// is not null, and frees it. A cancellation point.
///
/// Fails with `EDEADLK` if `t` is the calling thread, as glibc does, and with
/// `EINVAL` if it is detached and still running. (musl crashes for the
/// latter.)
///
/// # Safety
///
/// `t` must be a thread that has not been joined or ended detached, and `res`
/// null or valid for a write of a pointer.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_join(t: *mut Thread, res: *mut *mut c_void) -> c_int {
    // SAFETY: the caller's contract, with no time limit.
    unsafe { timed_join(t, res, null()) }
}

/// [`pthread_join`], but fails with `EBUSY` at once if `t` is still running.
///
/// # Safety
///
/// As [`pthread_join`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_tryjoin_np(t: *mut Thread, res: *mut *mut c_void) -> c_int {
    // SAFETY: the caller vouches for `t`.
    let state = unsafe { thread::state(t) };
    if state.detach_state.load(Ordering::SeqCst) == DT_JOINABLE {
        return errno::EBUSY;
    }
    // SAFETY: the caller's contract is `pthread_join`'s.
    unsafe { timed_join(t, res, null()) }
}

/// [`pthread_join`], but fails with `ETIMEDOUT` if `t` is still running at
/// the absolute time `*at` on `CLOCK_REALTIME`.
///
/// # Safety
///
/// As [`pthread_join`], and `at` must be null or valid for a read of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_timedjoin_np(
    t: *mut Thread,
    res: *mut *mut c_void,
    at: *const Timespec,
) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { timed_join(t, res, at) }
}

/// Makes thread `t` free its own memory when it ends. If it has already ended,
/// frees it now. Fails with `EINVAL` if it is already detached.
///
/// # Safety
///
/// `t` must be a thread that has not been joined or ended detached.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_detach(t: *mut Thread) -> c_int {
    // SAFETY: the caller vouches for `t`.
    let state = unsafe { thread::state(t) };
    match state.detach_state.compare_exchange(
        DT_JOINABLE,
        DT_DETACHED,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(_) => 0,
        Err(DT_DETACHED) => errno::EINVAL,
        Err(_) => {
            // It is exiting or has exited: free it as a joiner would.
            let old = cancel::set_state(cancel::DISABLE);
            // SAFETY: the caller vouches for `t`.
            let _ = unsafe { timed_join(t, null_mut(), null()) };
            let _ = cancel::set_state(old);
            0
        }
    }
}

/// The calling thread's id.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_self() -> *mut Thread {
    thread::current()
}

/// Nonzero if `a` and `b` are the same thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_equal(a: *mut Thread, b: *mut Thread) -> c_int {
    c_int::from(a == b)
}

/// The kernel's id for the calling thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gettid() -> c_int {
    // SAFETY: `gettid` takes no arguments.
    let tid = unsafe { syscall::syscall0(nr::GETTID) };
    tid as c_int
}

/// Sends `sig` to thread `t`, including the library's reserved signals.
///
/// Safe against `t` exiting concurrently: its id is read under its kill lock,
/// which it holds while clearing the id. A thread that has begun to exit
/// accepts any valid signal and ignores it, as POSIX asks for a thread that
/// has not yet been joined.
///
/// # Safety
///
/// `t` must be a thread that has not been joined or ended detached.
pub unsafe fn kill_thread(t: *mut Thread, sig: c_int) -> c_int {
    let mask = block_all_signals();
    // SAFETY: the caller vouches for `t`.
    let state = unsafe { thread::state(t) };
    state.kill_lock.acquire();
    // SAFETY: as above.
    let tid = unsafe { thread::tid(t) }.load(Ordering::SeqCst);
    let r = if tid != 0 {
        // SAFETY: `getpid` takes no arguments.
        let pid = unsafe { syscall::syscall0(nr::GETPID) };
        // SAFETY: `tgkill` reads no memory.
        let ret =
            unsafe { syscall::syscall3(nr::TGKILL, pid as usize, tid as usize, sig as usize) };
        errno::decode(ret).err().unwrap_or(0)
    } else if sig as c_uint >= sigset::NSIG as c_uint {
        errno::EINVAL
    } else {
        0
    };
    state.kill_lock.release();
    restore_signals(mask);
    r
}

/// Sends `sig` to thread `t`, or with 0 only checks that it could. The
/// library's reserved signals fail with `EINVAL`, as in glibc.
///
/// # Safety
///
/// `t` must be a thread that has not been joined or ended detached.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_kill(t: *mut Thread, sig: c_int) -> c_int {
    if sigset::is_reserved(sig) {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `t`.
    unsafe { kill_thread(t, sig) }
}

/// A `siginfo_t` as `sigqueue` fills it in: `signal.h`'s layout, 128 bytes.
#[repr(C)]
struct QueuedInfo {
    signo: c_int,
    errno: c_int,
    code: c_int,
    pad: c_int,
    pid: c_int,
    uid: c_uint,
    value: Sigval,
    rest: [u64; 12],
}

const _: () = assert!(size_of::<QueuedInfo>() == 128);
const _: () = assert!(offset_of!(QueuedInfo, value) == 24);

/// Sends `sig` with `value` to thread `t`, which finds it in `si_value`, with
/// `si_code` `SI_QUEUE`. A GNU extension.
///
/// # Safety
///
/// As [`pthread_kill`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_sigqueue(t: *mut Thread, sig: c_int, value: Sigval) -> c_int {
    if sigset::is_reserved(sig) {
        return errno::EINVAL;
    }
    // SAFETY: `getpid` and `getuid` take no arguments.
    let pid = unsafe { syscall::syscall0(nr::GETPID) } as c_int;
    // SAFETY: as above.
    let uid = unsafe { syscall::syscall0(nr::GETUID) } as c_uint;
    let info = QueuedInfo {
        signo: sig,
        errno: 0,
        code: SI_QUEUE,
        pad: 0,
        pid,
        uid,
        value,
        rest: [0; 12],
    };
    let mask = block_all_signals();
    // SAFETY: the caller vouches for `t`.
    let state = unsafe { thread::state(t) };
    state.kill_lock.acquire();
    // SAFETY: as above.
    let tid = unsafe { thread::tid(t) }.load(Ordering::SeqCst);
    let r = if tid == 0 {
        errno::ESRCH
    } else {
        // SAFETY: the kernel reads `info`, a live local.
        let ret = unsafe {
            syscall::syscall4(
                nr::RT_TGSIGQUEUEINFO,
                pid as usize,
                tid as usize,
                sig as usize,
                (&raw const info).addr(),
            )
        };
        errno::decode(ret).err().unwrap_or(0)
    };
    state.kill_lock.release();
    restore_signals(mask);
    r
}

/// Writes `/proc/self/task/<tid>/comm` and a NUL into `buf`, and returns it.
fn comm_path(tid: c_int, buf: &mut [u8; 40]) -> *const c_char {
    let mut out = 0;
    let mut put = |byte: u8| {
        if let Some(slot) = buf.get_mut(out) {
            *slot = byte;
            out += 1;
        }
    };
    for &byte in b"/proc/self/task/" {
        put(byte);
    }
    let mut digits = [0_u8; 10];
    let mut count = 0;
    let mut value = tid.unsigned_abs();
    loop {
        if let Some(slot) = digits.get_mut(count) {
            *slot = b'0' + (value % 10) as u8;
        }
        count += 1;
        value /= 10;
        if value == 0 || count == digits.len() {
            break;
        }
    }
    while count > 0 {
        count -= 1;
        put(digits.get(count).copied().unwrap_or(b'0'));
    }
    for &byte in b"/comm\0" {
        put(byte);
    }
    buf.as_ptr().cast()
}

/// The length of `name`, looking at no more than `max + 1` bytes.
///
/// # Safety
///
/// `name` must be readable up to its NUL or `max + 1` bytes.
unsafe fn bounded_len(name: *const c_char, max: usize) -> usize {
    let mut len = 0;
    // SAFETY: the caller vouches for the bytes before the NUL or the bound.
    while len <= max && unsafe { name.wrapping_add(len).read() } != 0 {
        len += 1;
    }
    len
}

/// Names thread `t` `name`, which the kernel shows in `/proc` and debuggers.
/// Fails with `ERANGE` for a name longer than 15 bytes.
///
/// The calling thread names itself with `prctl`. Another thread's name is
/// written to `/proc/self/task/<tid>/comm`, as musl and glibc do.
///
/// # Safety
///
/// `t` must be a running thread, and `name` a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setname_np(t: *mut Thread, name: *const c_char) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { bounded_len(name, NAME_MAX) };
    if len > NAME_MAX {
        return errno::ERANGE;
    }
    if t == thread::current() {
        // SAFETY: the kernel reads up to 16 bytes of the name.
        let ret = unsafe { syscall::syscall2(nr::PRCTL, PR_SET_NAME, name.addr()) };
        return errno::decode(ret).err().unwrap_or(0);
    }
    let mut path = [0_u8; 40];
    // SAFETY: the caller vouches for `t`.
    let path = comm_path(unsafe { thread::tid(t) }.load(Ordering::SeqCst), &mut path);
    // SAFETY: the kernel reads the NUL-terminated path.
    let fd = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            AT_FDCWD as usize,
            path.addr(),
            O_WRONLY_CLOEXEC,
            0,
        )
    };
    let fd = match errno::decode(fd) {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    // SAFETY: the kernel reads `len` bytes of the name.
    let wrote = unsafe { syscall::syscall3(nr::WRITE, fd, name.addr(), len) };
    // SAFETY: the descriptor is this function's.
    let _ = unsafe { syscall::syscall2(nr::CLOSE, fd, 0) };
    errno::decode(wrote).err().unwrap_or(0)
}

/// Stores thread `t`'s name, NUL-terminated, in the `len` bytes at `name`.
/// Fails with `ERANGE` if `len` is below 16.
///
/// # Safety
///
/// `t` must be a running thread, and `name` valid for writes of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_getname_np(
    t: *mut Thread,
    name: *mut c_char,
    len: usize,
) -> c_int {
    if len <= NAME_MAX {
        return errno::ERANGE;
    }
    if t == thread::current() {
        // SAFETY: the kernel writes 16 bytes, which the caller has room for.
        let ret = unsafe { syscall::syscall2(nr::PRCTL, PR_GET_NAME, name.addr()) };
        return errno::decode(ret).err().unwrap_or(0);
    }
    let mut path = [0_u8; 40];
    // SAFETY: the caller vouches for `t`.
    let path = comm_path(unsafe { thread::tid(t) }.load(Ordering::SeqCst), &mut path);
    // SAFETY: the kernel reads the NUL-terminated path.
    let fd = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            AT_FDCWD as usize,
            path.addr(),
            O_RDONLY_CLOEXEC,
            0,
        )
    };
    let fd = match errno::decode(fd) {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    // SAFETY: the kernel writes at most `len` bytes, which the caller has.
    let read = unsafe { syscall::syscall3(nr::READ, fd, name.addr(), len) };
    // SAFETY: the descriptor is this function's.
    let _ = unsafe { syscall::syscall2(nr::CLOSE, fd, 0) };
    match errno::decode(read) {
        Ok(count) => {
            // The name ends in a newline, which becomes the NUL.
            if let Some(last) = count.checked_sub(1) {
                // SAFETY: `last` is below the count the kernel wrote.
                unsafe { name.wrapping_add(last).write(0) };
            }
            0
        }
        Err(error) => error,
    }
}

/// A registration made by [`pthread_atfork`], and the list it is on.
///
/// The list is doubly linked so that it can be walked in either direction:
/// POSIX has the `prepare` handlers run in the reverse of the order they were
/// registered in, and the `parent` and `child` handlers in that order. New
/// registrations go on the front, so `older` walks the first way and `newer`
/// the second.
///
/// A node is never removed, because POSIX has no way to unregister one, so a
/// node once linked is read-only and lives until the process ends.
struct AtforkHandlers {
    prepare: Option<extern "C" fn()>,
    parent: Option<extern "C" fn()>,
    child: Option<extern "C" fn()>,
    /// The registration made before this one.
    older: *mut AtforkHandlers,
    /// The registration made after this one.
    newer: *mut AtforkHandlers,
}

/// The most recent registration, or null.
static ATFORK_NEWEST: AtomicUsize = AtomicUsize::new(0);
/// The first registration, or null.
static ATFORK_OLDEST: AtomicUsize = AtomicUsize::new(0);
/// Guards the two above, and is held from the `prepare` handlers until the
/// `parent` or `child` handlers have run, so that no thread registers a
/// handler in the middle of a fork.
static ATFORK_LOCK: crate::lock::SpinLock = crate::lock::SpinLock::new();

/// Registers up to three functions to run around a [`crate::process::fork`]:
/// `prepare` in the forking thread before the fork, `parent` in that thread
/// afterwards, and `child` in the new process. Any of them may be null.
///
/// Returns 0, or `ENOMEM` if the registration could not be allocated. It is
/// never undone: POSIX gives no way to remove a handler.
///
/// A library registers these to take its locks before a fork and release them
/// on both sides, because the child of a fork has only the forking thread, and
/// a lock another thread held is held for ever there.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_atfork(
    prepare: Option<extern "C" fn()>,
    parent: Option<extern "C" fn()>,
    child: Option<extern "C" fn()>,
) -> c_int {
    // This library's own allocator, not the process's: a node never leaves
    // the library, and a program's allocator registers its fork handlers from
    // inside itself -- PartitionAlloc does, holding its lock -- so the
    // process's `malloc` here would call back into it and wait on that lock
    // for ever. glibc keeps its first handlers in static storage for the same
    // reason.
    let node = crate::malloc::own_malloc(size_of::<AtforkHandlers>()).cast::<AtforkHandlers>();
    if node.is_null() {
        return errno::ENOMEM;
    }
    ATFORK_LOCK.acquire();
    let newest = ATFORK_NEWEST.load(Ordering::Relaxed);
    // SAFETY: `node` is this thread's fresh allocation, large enough and
    // aligned for the structure, and nothing else refers to it yet.
    unsafe {
        node.write(AtforkHandlers {
            prepare,
            parent,
            child,
            older: with_exposed_provenance_mut(newest),
            newer: null_mut(),
        });
    }
    let previous = with_exposed_provenance_mut::<AtforkHandlers>(newest);
    if previous.is_null() {
        ATFORK_OLDEST.store(node.addr(), Ordering::Relaxed);
    } else {
        // SAFETY: the list holds its nodes for the life of the process, and
        // the lock keeps this the only writer.
        unsafe { (*previous).newer = node };
    }
    ATFORK_NEWEST.store(node.addr(), Ordering::Relaxed);
    ATFORK_LOCK.release();
    0
}

/// Runs the `prepare` handlers, newest first, and leaves [`ATFORK_LOCK`] held
/// for [`run_atfork_parent`] or [`run_atfork_child`] to release.
///
/// Called by `fork` before the `clone`.
pub(crate) fn run_atfork_prepare() {
    ATFORK_LOCK.acquire();
    let mut at =
        with_exposed_provenance_mut::<AtforkHandlers>(ATFORK_NEWEST.load(Ordering::Relaxed));
    while !at.is_null() {
        // SAFETY: a linked node is read-only and lives as long as the process.
        let node = unsafe { &*at };
        if let Some(prepare) = node.prepare {
            prepare();
        }
        at = node.older;
    }
}

/// Runs the `parent` handlers, oldest first, and releases the lock
/// [`run_atfork_prepare`] took. Called in the forking process after the
/// `clone`, whether it succeeded or not.
pub(crate) fn run_atfork_parent() {
    run_atfork_after(false);
}

/// Runs the `child` handlers, oldest first, in the new process.
///
/// The lock is reset rather than released: the child is a copy made while the
/// forking thread held it, so here it is held by the thread running this, and
/// resetting it is that thread releasing it. No other thread survives a fork
/// to be woken.
pub(crate) fn run_atfork_child() {
    run_atfork_after(true);
}

/// What [`run_atfork_parent`] and [`run_atfork_child`] share.
fn run_atfork_after(child: bool) {
    let mut at =
        with_exposed_provenance_mut::<AtforkHandlers>(ATFORK_OLDEST.load(Ordering::Relaxed));
    while !at.is_null() {
        // SAFETY: a linked node is read-only and lives as long as the process.
        let node = unsafe { &*at };
        let hook = if child { node.child } else { node.parent };
        if let Some(hook) = hook {
            hook();
        }
        at = node.newer;
    }
    if child {
        ATFORK_LOCK.reset();
    } else {
        ATFORK_LOCK.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comm_path_names_the_thread() {
        let mut buf = [0_u8; 40];
        let _ = comm_path(1234, &mut buf);
        assert!(buf.starts_with(b"/proc/self/task/1234/comm\0"));
        let _ = comm_path(2_147_483_647, &mut buf);
        assert!(buf.starts_with(b"/proc/self/task/2147483647/comm\0"));
    }

    #[test]
    fn the_reserved_signals_are_the_ones_pthread_create_unblocks() {
        assert_eq!(INTERNAL_SIGNALS, 0b11 << 32);
        assert_eq!(APP_SIGNALS & INTERNAL_SIGNALS, 0);
        assert!(sigset::is_reserved(SIGCANCEL) && sigset::is_reserved(SIGSYNCCALL));
    }

    #[test]
    fn a_bounded_length_stops_at_the_bound() {
        // SAFETY: the strings are NUL-terminated.
        assert_eq!(unsafe { bounded_len(c"abc".as_ptr(), 15) }, 3);
        let long = c"0123456789abcdefghij";
        // SAFETY: the strings are NUL-terminated.
        assert_eq!(unsafe { bounded_len(long.as_ptr(), 15) }, 16);
    }
}
