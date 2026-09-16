//! Thread cancellation: the cancel state and type, and the cleanup handlers
//! `pthread_cleanup_push` registers.
//!
//! musl's header expands `pthread_cleanup_push` into a `struct __ptcb` on the
//! caller's stack and a call to [`_pthread_cleanup_push`], which links it into
//! the thread's list. `pthread_exit` runs the list, innermost first.
//!
//! # The masked state
//!
//! Besides `PTHREAD_CANCEL_ENABLE` and `PTHREAD_CANCEL_DISABLE`, a thread may
//! be in musl's `PTHREAD_CANCEL_MASKED` state, which only the library enters.
//! A cancellation point that finds a cancellation pending in that state
//! reports `ECANCELED` instead of acting, and disables cancellation. The
//! condition variable wait uses it to reacquire its mutex before acting on a
//! cancellation.
//!
//! # Requests
//!
//! This is musl's design. `pthread_cancel` sets the target's flag and sends
//! it [`SIGCANCEL`]. Every cancellation point makes its system call through
//! [`syscall_cp`], which checks the flag and makes the call inside a window of
//! assembly the signal handler recognises by its program counter. A signal
//! that arrives before the call or while it blocks moves the thread to the
//! cancellation instead. One that arrives after the call returned leaves the
//! result to be reported. Either way no request falls between the check and
//! the call.

use core::ffi::{c_int, c_ulong, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::errno;
use crate::sigaction::{self, KernelSigaction, SA_ONSTACK, SA_RESTART, SA_RESTORER, SA_SIGINFO};
use crate::syscall::{self, nr};
use crate::thread::Thread;

/// `PTHREAD_CANCEL_ENABLE`.
pub const ENABLE: u8 = 0;
/// `PTHREAD_CANCEL_DISABLE`.
pub const DISABLE: u8 = 1;
/// `PTHREAD_CANCEL_MASKED`: musl's state for the library's own use.
pub const MASKED: u8 = 2;

/// `PTHREAD_CANCEL_DEFERRED`.
pub const DEFERRED: u8 = 0;
/// `PTHREAD_CANCEL_ASYNCHRONOUS`.
pub const ASYNCHRONOUS: u8 = 1;

/// `PTHREAD_CANCELED`: what a cancelled thread's join reports.
pub const CANCELED: *mut c_void = core::ptr::without_provenance_mut(usize::MAX);

/// A cleanup handler.
pub type Handler = unsafe extern "C" fn(*mut c_void);

/// `struct __ptcb`, from `pthread.h`: one registered cleanup handler, on the
/// stack of the code that pushed it.
#[repr(C)]
#[derive(Debug)]
pub struct Cleanup {
    /// `__f`: the handler.
    pub func: Option<Handler>,
    /// `__x`: its argument.
    pub arg: *mut c_void,
    /// `__next`: the handler pushed before this one.
    pub next: *mut Cleanup,
}

const _: () = assert!(size_of::<Cleanup>() == 24);
const _: () = assert!(offset_of!(Cleanup, next) == 16);

/// Sets the calling thread's cancel state to `new`, one of [`ENABLE`],
/// [`DISABLE`] and [`MASKED`], and returns the old one.
#[cfg(not(test))]
pub fn set_state(new: u8) -> u8 {
    crate::thread::me()
        .cancel_disable
        .swap(new, Ordering::Relaxed)
}

/// In unit tests there is no control block, and nothing is cancelled.
#[cfg(test)]
pub fn set_state(new: u8) -> u8 {
    let _ = new;
    ENABLE
}

/// Sets the calling thread's cancel state, storing the old one in `*old` if
/// `old` is not null. Fails with `EINVAL` for an unknown state.
///
/// # Safety
///
/// `old` must be null or valid for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setcancelstate(new: c_int, old: *mut c_int) -> c_int {
    let Ok(new) = u8::try_from(new) else {
        return errno::EINVAL;
    };
    if new > MASKED {
        return errno::EINVAL;
    }
    let previous = set_state(new);
    if !old.is_null() {
        // SAFETY: the caller vouches for a non-null `old`.
        unsafe { old.write(c_int::from(previous)) };
    }
    0
}

/// Sets the calling thread's cancel type, storing the old one in `*old` if
/// `old` is not null. Fails with `EINVAL` for an unknown type.
///
/// # Safety
///
/// `old` must be null or valid for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setcanceltype(new: c_int, old: *mut c_int) -> c_int {
    let Ok(new) = u8::try_from(new) else {
        return errno::EINVAL;
    };
    if new > ASYNCHRONOUS {
        return errno::EINVAL;
    }
    let previous = crate::thread::me()
        .cancel_async
        .swap(new, Ordering::Relaxed);
    if !old.is_null() {
        // SAFETY: the caller vouches for a non-null `old`.
        unsafe { old.write(c_int::from(previous)) };
    }
    0
}

/// Registers `func(arg)` as the calling thread's innermost cleanup handler,
/// in the `struct __ptcb` at `cb`. `pthread_cleanup_push` expands to this.
///
/// # Safety
///
/// `cb` must be valid for writes of a `struct __ptcb`, and stay so until the
/// matching [`_pthread_cleanup_pop`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _pthread_cleanup_push(
    cb: *mut Cleanup,
    func: Option<Handler>,
    arg: *mut c_void,
) {
    let state = crate::thread::me();
    let next = state.cancel_buf.load(Ordering::Relaxed);
    // SAFETY: the caller vouches for `cb`.
    unsafe { cb.write(Cleanup { func, arg, next }) };
    state.cancel_buf.store(cb, Ordering::Relaxed);
}

/// Removes the innermost cleanup handler, the one at `cb`, and runs it if
/// `run` is nonzero. `pthread_cleanup_pop` expands to this.
///
/// # Safety
///
/// `cb` must be the handler the matching [`_pthread_cleanup_push`] filled in.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _pthread_cleanup_pop(cb: *mut Cleanup, run: c_int) {
    // SAFETY: the caller vouches for `cb`, which push filled in.
    let Cleanup { func, arg, next } = unsafe { cb.read() };
    crate::thread::me()
        .cancel_buf
        .store(next, Ordering::Relaxed);
    if run != 0
        && let Some(func) = func
    {
        // SAFETY: the program registered the handler to be run with `arg`.
        unsafe { func(arg) };
    }
}

/// Runs and removes the calling thread's cleanup handlers, innermost first.
/// `pthread_exit` calls this.
pub fn run_cleanup_handlers() {
    let state = crate::thread::me();
    loop {
        let cb = state.cancel_buf.load(Ordering::Relaxed);
        if cb.is_null() {
            return;
        }
        // SAFETY: every entry on the list is a live `struct __ptcb` its pusher
        // has not yet popped, whose frame is still on this thread's stack.
        let Cleanup { func, arg, next } = unsafe { cb.read() };
        state.cancel_buf.store(next, Ordering::Relaxed);
        if let Some(func) = func {
            // SAFETY: the program registered the handler to be run with
            // `arg` if the thread exits.
            unsafe { func(arg) };
        }
    }
}

/// The signal `pthread_cancel` sends: musl's `SIGCANCEL`, the second of the
/// three signals the library reserves.
pub const SIGCANCEL: c_int = 33;

/// Acts on a pending cancellation, if the calling thread's cancel state is
/// enabled: the thread exits as if with `pthread_exit(PTHREAD_CANCELED)`.
pub fn testcancel() {
    let state = crate::thread::me();
    if state.cancel.load(Ordering::SeqCst) != 0
        && state.cancel_disable.load(Ordering::SeqCst) == ENABLE
    {
        crate::pthread::pthread_exit(CANCELED);
    }
}

/// Acts on a cancellation found at a cancellation point: musl's `__cancel`.
/// An enabled or asynchronous thread exits. A thread in [`MASKED`] state gets
/// `-ECANCELED` back instead, and cancellation is disabled, so the library
/// function that masked it can finish its work before it acts.
extern "C" fn cancel_now() -> isize {
    let state = crate::thread::me();
    if state.cancel_disable.load(Ordering::SeqCst) == ENABLE
        || state.cancel_async.load(Ordering::SeqCst) != 0
    {
        crate::pthread::pthread_exit(CANCELED);
    }
    state.cancel_disable.store(DISABLE, Ordering::SeqCst);
    -(errno::ECANCELED as isize)
}

// The system call at a cancellation point: musl's `__syscall_cp_asm`.
//
// `rdi` is the thread's cancel flag, `rsi` the call's number, and the call's
// six arguments follow in `rdx`, `rcx`, `r8`, `r9` and the two stack slots.
// Between `__ferrousli_cp_begin` and `__ferrousli_cp_end` the flag has been
// read and found clear, and the call has not yet returned. A `SIGCANCEL` that
// lands there, blocked in the call or about to make it, finds its program
// counter in that range, and the handler moves it to
// `__ferrousli_cp_cancel`, so the thread acts on the cancellation instead of
// on the call's result. At `__ferrousli_cp_end` the call has completed, and
// its effect, a read that consumed bytes, say, is reported rather than lost.
unsafe extern "C" {
    fn __ferrousli_syscall_cp_asm(
        cancel: *const AtomicI32,
        number: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> isize;
    static __ferrousli_cp_begin: u8;
    static __ferrousli_cp_end: u8;
    static __ferrousli_cp_cancel: u8;
}

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".pushsection .text.__ferrousli_syscall_cp_asm,\"ax\",@progbits",
    ".p2align 4",
    ".hidden __ferrousli_syscall_cp_asm",
    ".hidden __ferrousli_cp_begin",
    ".hidden __ferrousli_cp_end",
    ".hidden __ferrousli_cp_cancel",
    ".globl __ferrousli_syscall_cp_asm",
    ".globl __ferrousli_cp_begin",
    ".globl __ferrousli_cp_end",
    ".globl __ferrousli_cp_cancel",
    ".type __ferrousli_syscall_cp_asm, @function",
    "__ferrousli_syscall_cp_asm:",
    "__ferrousli_cp_begin:",
    "mov eax, dword ptr [rdi]",
    "test eax, eax",
    "jnz __ferrousli_cp_cancel",
    "mov r11, rdi",
    "mov rax, rsi",
    "mov rdi, rdx",
    "mov rsi, rcx",
    "mov rdx, r8",
    "mov r10, r9",
    "mov r8, qword ptr [rsp + 8]",
    "mov r9, qword ptr [rsp + 16]",
    "mov qword ptr [rsp + 8], r11",
    "syscall",
    "__ferrousli_cp_end:",
    "ret",
    "__ferrousli_cp_cancel:",
    "jmp {cancel}",
    ".size __ferrousli_syscall_cp_asm, . - __ferrousli_syscall_cp_asm",
    ".popsection",
    cancel = sym cancel_now,
);

/// The offset of the saved program counter, `uc_mcontext.gregs[REG_RIP]`, in
/// the kernel's `struct ucontext` on x86-64.
const UC_RIP: usize = 168;
/// The offset of `uc_sigmask` in the kernel's `struct ucontext` on x86-64.
const UC_SIGMASK: usize = 296;

/// `rt_sigprocmask`'s `SIG_SETMASK`.
const SIG_SETMASK: usize = 2;

/// `SIGCANCEL`'s handler: musl's `cancel_handler`.
///
/// It does nothing for a thread whose cancellation is disabled or was not
/// asked for. Otherwise it leaves `SIGCANCEL` blocked for the rest of the
/// thread, whose flag is enough from now on, and acts at once if the thread
/// is asynchronous or is inside a cancellation point's window. Anywhere else
/// it sends itself the signal again, which stays pending and blocked, and the
/// next cancellation point acts on the flag.
unsafe extern "C" fn cancel_handler(_sig: c_int, _info: *mut c_void, context: *mut c_void) {
    let state = crate::thread::me();
    if state.cancel.load(Ordering::SeqCst) == 0
        || state.cancel_disable.load(Ordering::SeqCst) == DISABLE
    {
        return;
    }
    let context = context.cast::<u8>();
    let mask_at = context.wrapping_add(UC_SIGMASK).cast::<[u8; 8]>();
    // SAFETY: the kernel passed its `ucontext` for this frame, whose mask is
    // restored when the handler returns.
    let mask = u64::from_ne_bytes(unsafe { mask_at.read() }) | (1_u64 << (SIGCANCEL - 1));
    // SAFETY: as above.
    unsafe { mask_at.write(mask.to_ne_bytes()) };
    if state.cancel_async.load(Ordering::SeqCst) != 0 {
        // SAFETY: the kernel reads one word of the mask, a live local.
        let _ = unsafe {
            syscall::syscall4(
                nr::RT_SIGPROCMASK,
                SIG_SETMASK,
                (&raw const mask).addr(),
                0,
                size_of::<u64>(),
            )
        };
        let _ = cancel_now();
    }
    let rip_at = context.wrapping_add(UC_RIP).cast::<[u8; 8]>();
    // SAFETY: as above.
    let pc = usize::from_ne_bytes(unsafe { rip_at.read() });
    let begin = (&raw const __ferrousli_cp_begin).addr();
    let end = (&raw const __ferrousli_cp_end).addr();
    if (begin..end).contains(&pc) {
        // SAFETY: as above; the frame resumes at the window's exit, which
        // takes no state from the call it skips.
        unsafe { rip_at.write((&raw const __ferrousli_cp_cancel).addr().to_ne_bytes()) };
        return;
    }
    // SAFETY: `tkill` reads no memory.
    let _ = unsafe {
        syscall::syscall2(
            nr::TKILL,
            crate::thread::my_tid() as usize,
            SIGCANCEL as usize,
        )
    };
}

/// Installs [`cancel_handler`] once, before the first cancellation request.
fn install_handler() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let action = KernelSigaction {
        handler: (cancel_handler as unsafe extern "C" fn(c_int, *mut c_void, *mut c_void)
            as *const ())
            .addr(),
        flags: (SA_SIGINFO | SA_RESTART | SA_ONSTACK | SA_RESTORER) as c_ulong,
        restorer: sigaction::restorer(),
        mask: !0,
    };
    // SAFETY: the handler takes `SIGCANCEL` on any thread, and the restorer
    // calls `rt_sigreturn`.
    let _ = unsafe { sigaction::kernel_sigaction(SIGCANCEL, Some(&action), None) };
}

/// Makes a system call that is a cancellation point: musl's
/// `__syscall_cp_c`. A thread with cancellation disabled makes the plain
/// call, as does any thread closing a descriptor with it masked, since a
/// `close` interrupted is a descriptor lost. Otherwise the call is made in
/// the cancellation window, and an `EINTR` from it acts on a cancellation
/// that arrived during it.
///
/// # Safety
///
/// As the system call.
#[cfg(not(test))]
pub unsafe fn syscall_cp(
    number: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> isize {
    let state = crate::thread::me();
    let disable = state.cancel_disable.load(Ordering::SeqCst);
    if disable == DISABLE || (disable != ENABLE && number == nr::CLOSE) {
        // SAFETY: the caller vouches for the call.
        return unsafe { syscall::syscall6(number, a0, a1, a2, a3, a4, a5) };
    }
    // SAFETY: the caller vouches for the call, and the flag is the calling
    // thread's, which outlives it.
    let ret = unsafe {
        __ferrousli_syscall_cp_asm(&raw const state.cancel, number, a0, a1, a2, a3, a4, a5)
    };
    if ret == -(errno::EINTR as isize)
        && number != nr::CLOSE
        && state.cancel.load(Ordering::SeqCst) != 0
        && state.cancel_disable.load(Ordering::SeqCst) != DISABLE
    {
        return cancel_now();
    }
    ret
}

/// In unit tests there is no control block, and nothing is cancelled: the
/// plain system call.
///
/// # Safety
///
/// As the system call.
#[cfg(test)]
pub unsafe fn syscall_cp(
    number: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> isize {
    // SAFETY: the caller vouches for the call.
    unsafe { syscall::syscall6(number, a0, a1, a2, a3, a4, a5) }
}

/// Asks thread `t` to be cancelled. It acts at its next cancellation point
/// while its cancellation is enabled, at once if it is asynchronous, and not
/// before it enables cancellation otherwise. The calling thread cancelling
/// itself asynchronously ends at once.
///
/// # Safety
///
/// `t` must be a thread that has not been joined or ended detached.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_cancel(t: *mut Thread) -> c_int {
    install_handler();
    // SAFETY: the caller vouches for `t`.
    unsafe { crate::thread::state(t) }
        .cancel
        .store(1, Ordering::SeqCst);
    if t == crate::thread::current() {
        let state = crate::thread::me();
        if state.cancel_disable.load(Ordering::SeqCst) == ENABLE
            && state.cancel_async.load(Ordering::SeqCst) != 0
        {
            crate::pthread::pthread_exit(CANCELED);
        }
        return 0;
    }
    // SAFETY: the caller vouches for `t`.
    unsafe { crate::pthread::kill_thread(t, SIGCANCEL) }
}

/// Cancellation disabled for the calling thread until this is dropped, when
/// the state it replaced comes back: the library's own guard, as musl uses
/// `pthread_setcancelstate`, around work that makes cancellation points'
/// system calls on the way to finishing something a cancellation must not
/// cut in half, such as a DNS exchange or a forked child's setup.
#[derive(Debug)]
#[must_use = "cancellation is enabled again when the guard is dropped"]
pub struct Disabled {
    /// The state to restore.
    previous: u8,
}

/// Disables cancellation for the calling thread while the guard lives.
pub fn disable() -> Disabled {
    Disabled {
        previous: set_state(DISABLE),
    }
}

impl Drop for Disabled {
    fn drop(&mut self) {
        let _ = set_state(self.previous);
    }
}

/// A cancellation point and nothing else.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_testcancel() {
    testcancel();
}

/// Removes nothing: kept so that a null list head reads as empty.
#[allow(dead_code, reason = "documents the empty list")]
const EMPTY: *mut Cleanup = null_mut();
