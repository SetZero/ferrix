//! Leaving the process: `exit` and the handlers it runs, and `_Exit`.
//!
//! `exit` runs, in order: the handlers registered with `atexit` and
//! `__cxa_atexit`, newest first; the program's `.fini_array`, last entry first;
//! then `_Exit`. musl uses the same order, and in glibc the effect is the same,
//! because glibc registers the `.fini_array` walk as the first handler.

use core::ffi::{c_int, c_void};
use core::mem::transmute;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::lock::SpinLock;
use crate::syscall;

/// A handler as `__cxa_atexit` takes it.
type Callback = unsafe extern "C" fn(*mut c_void);

/// How many handlers can be registered. POSIX requires at least 32. Until
/// there is `malloc` there is room for exactly that many.
const CAPACITY: usize = 32;

static LOCK: SpinLock = SpinLock::new();
/// Each entry is a [`Callback`], stored as a data pointer.
static FUNCS: [AtomicPtr<()>; CAPACITY] = [const { AtomicPtr::new(null_mut()) }; CAPACITY];
static ARGS: [AtomicPtr<c_void>; CAPACITY] = [const { AtomicPtr::new(null_mut()) }; CAPACITY];
/// How many entries are registered and not yet run.
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Registers `func` to be called with `arg` at exit. `dso` names the module
/// that registered it. With one module in a static program, it is not needed.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __cxa_atexit(
    func: Option<Callback>,
    arg: *mut c_void,
    _dso: *mut c_void,
) -> c_int {
    let Some(func) = func else {
        return -1;
    };
    let _guard = LOCK.lock();
    let count = COUNT.load(Ordering::Relaxed);
    let (Some(func_slot), Some(arg_slot)) = (FUNCS.get(count), ARGS.get(count)) else {
        return -1;
    };
    func_slot.store(func as *mut (), Ordering::Relaxed);
    arg_slot.store(arg, Ordering::Relaxed);
    COUNT.store(count + 1, Ordering::Relaxed);
    0
}

/// Registers `func` to be called at exit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn atexit(func: Option<unsafe extern "C" fn()>) -> c_int {
    let Some(func) = func else {
        return -1;
    };
    __cxa_atexit(Some(call_plain), func as *mut c_void, null_mut())
}

/// Calls a handler `atexit` registered, which it passed as the argument.
unsafe extern "C" fn call_plain(arg: *mut c_void) {
    // SAFETY: `atexit` stored an `unsafe extern "C" fn()` as the argument.
    let func = unsafe { transmute::<*mut c_void, unsafe extern "C" fn()>(arg) };
    // SAFETY: the program registered it to be called at exit.
    unsafe { func() };
}

/// Runs a module's handlers when it is unloaded. A static program has one
/// module, and `exit` runs its handlers.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __cxa_finalize(_dso: *mut c_void) {}

/// Runs every registered handler, newest first. A handler may register another,
/// which runs next, so the lock is not held while one runs.
pub fn run_handlers() {
    loop {
        let (func, arg) = {
            let _guard = LOCK.lock();
            let Some(last) = COUNT.load(Ordering::Relaxed).checked_sub(1) else {
                return;
            };
            COUNT.store(last, Ordering::Relaxed);
            let (Some(func), Some(arg)) = (FUNCS.get(last), ARGS.get(last)) else {
                return;
            };
            (func.load(Ordering::Relaxed), arg.load(Ordering::Relaxed))
        };
        // SAFETY: only `__cxa_atexit` stores into `FUNCS`, and only a
        // `Callback`.
        let func = unsafe { transmute::<*mut (), Callback>(func) };
        // SAFETY: the program registered it to be called at exit, with this
        // argument.
        unsafe { func(arg) };
    }
}

/// Ends the process with `status`, after running the exit handlers and the
/// program's destructors.
///
/// Buffered output is not flushed yet, because there is none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn exit(status: c_int) -> ! {
    run_handlers();
    // SAFETY: this is `exit`, which runs the destructors once, after the
    // handlers.
    #[cfg(not(test))]
    unsafe {
        crate::start::run_fini();
    }
    _Exit(status)
}

/// Ends the process with `status` at once.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_snake_case, reason = "C names it")]
pub extern "C" fn _Exit(status: c_int) -> ! {
    syscall::exit_group(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU32;

    static CALLS: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn record(arg: *mut c_void) {
        // Shift in each argument, so the order of calls shows in the value.
        let _ = CALLS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |calls| {
            Some(calls * 10 + arg.addr() as u32)
        });
    }

    #[test]
    fn handlers_run_newest_first_and_only_once() {
        for arg in 1..=3_usize {
            assert_eq!(
                __cxa_atexit(
                    Some(record),
                    core::ptr::without_provenance_mut(arg),
                    null_mut()
                ),
                0
            );
        }
        run_handlers();
        run_handlers();
        assert_eq!(CALLS.load(Ordering::Relaxed), 321);
    }
}
