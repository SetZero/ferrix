//! Leaving the process: `exit` and the handlers it runs, and `_Exit`.
//!
//! `exit` runs, in order: the handlers registered with `atexit` and
//! `__cxa_atexit`, newest first; the program's `.fini_array`, last entry first;
//! then `_Exit`. musl uses the same order, and in glibc the effect is the same,
//! because glibc registers the `.fini_array` walk as the first handler.
//!
//! The first 32 handlers go into static slots, so that registering them never
//! allocates and cannot fail. The rest go into an array from `malloc`. The
//! static slots fill first and empty last, so the array holds entries only
//! while every slot is full, and taking the newest entry from the array
//! before the slots keeps the order newest first.

use core::ffi::{c_int, c_void};
use core::mem::{size_of, transmute};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::lock::SpinLock;
use crate::malloc::realloc;
use crate::syscall;

/// A handler as `__cxa_atexit` takes it.
type Callback = unsafe extern "C" fn(*mut c_void);

/// How many handlers fit without allocating: the 32 POSIX requires.
const CAPACITY: usize = 32;

/// A handler beyond the static slots.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// A [`Callback`], stored as a data pointer.
    func: *mut (),
    arg: *mut c_void,
}

static LOCK: SpinLock = SpinLock::new();
/// Each entry is a [`Callback`], stored as a data pointer.
static FUNCS: [AtomicPtr<()>; CAPACITY] = [const { AtomicPtr::new(null_mut()) }; CAPACITY];
static ARGS: [AtomicPtr<c_void>; CAPACITY] = [const { AtomicPtr::new(null_mut()) }; CAPACITY];
/// How many static slots are registered and not yet run.
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// The handlers beyond the static slots, in an array from `malloc`.
static EXTRA: AtomicPtr<Entry> = AtomicPtr::new(null_mut());
/// How many entries `EXTRA` holds.
static EXTRA_LEN: AtomicUsize = AtomicUsize::new(0);
/// How many entries `EXTRA` has room for.
static EXTRA_CAP: AtomicUsize = AtomicUsize::new(0);

/// Registers `func` to be called with `arg` at exit. `dso` names the module
/// that registered it. With one module in a static program, it is not needed.
///
/// Fails, returning -1, only when a handler beyond the 32 static slots cannot
/// be given memory.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __cxa_atexit(
    func: Option<Callback>,
    arg: *mut c_void,
    _dso: *mut c_void,
) -> c_int {
    let Some(func) = func else {
        return -1;
    };
    // Registration is rare, so growing the array while holding the lock, and
    // making anyone else waiting spin through `malloc`'s system call, is an
    // acceptable price for keeping the order simple.
    let _guard = LOCK.lock();
    let count = COUNT.load(Ordering::Relaxed);
    if let (Some(func_slot), Some(arg_slot)) = (FUNCS.get(count), ARGS.get(count)) {
        func_slot.store(func as *mut (), Ordering::Relaxed);
        arg_slot.store(arg, Ordering::Relaxed);
        COUNT.store(count + 1, Ordering::Relaxed);
        return 0;
    }
    push_extra(Entry {
        func: func as *mut (),
        arg,
    })
}

/// Appends `entry` to the array beyond the static slots, growing it if full.
/// The lock must be held.
fn push_extra(entry: Entry) -> c_int {
    let len = EXTRA_LEN.load(Ordering::Relaxed);
    let cap = EXTRA_CAP.load(Ordering::Relaxed);
    let mut array = EXTRA.load(Ordering::Relaxed);
    if len == cap {
        let Some(new_cap) = cap.checked_mul(2).map(|doubled| doubled.max(CAPACITY)) else {
            return -1;
        };
        let Some(bytes) = new_cap.checked_mul(size_of::<Entry>()) else {
            return -1;
        };
        // SAFETY: the array is null or came from `malloc`, and only the lock's
        // holder uses it.
        let grown = unsafe { realloc(array.cast(), bytes) }.cast::<Entry>();
        if grown.is_null() {
            return -1;
        }
        array = grown;
        EXTRA.store(grown, Ordering::Relaxed);
        EXTRA_CAP.store(new_cap, Ordering::Relaxed);
    }
    // SAFETY: the array has room for `len + 1` entries.
    unsafe { array.wrapping_add(len).write(entry) };
    EXTRA_LEN.store(len + 1, Ordering::Relaxed);
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

/// Takes the newest registered handler off its list, if there is one.
fn take_newest() -> Option<(*mut (), *mut c_void)> {
    let _guard = LOCK.lock();
    if let Some(last) = EXTRA_LEN.load(Ordering::Relaxed).checked_sub(1) {
        EXTRA_LEN.store(last, Ordering::Relaxed);
        // SAFETY: the array holds `last + 1` entries.
        let entry = unsafe { EXTRA.load(Ordering::Relaxed).wrapping_add(last).read() };
        return Some((entry.func, entry.arg));
    }
    let last = COUNT.load(Ordering::Relaxed).checked_sub(1)?;
    COUNT.store(last, Ordering::Relaxed);
    let (Some(func), Some(arg)) = (FUNCS.get(last), ARGS.get(last)) else {
        return None;
    };
    Some((func.load(Ordering::Relaxed), arg.load(Ordering::Relaxed)))
}

/// Runs every registered handler, newest first. A handler may register another,
/// which runs next, so the lock is not held while one runs.
pub fn run_handlers() {
    while let Some((func, arg)) = take_newest() {
        // SAFETY: only `__cxa_atexit` stores handlers, and only `Callback`s.
        let func = unsafe { transmute::<*mut (), Callback>(func) };
        // SAFETY: the program registered it to be called at exit, with this
        // argument.
        unsafe { func(arg) };
    }
}

/// Ends the process with `status`, after running the exit handlers and the
/// program's destructors.
///
/// Every stream's buffered output is flushed last, after the destructors, as
/// musl does, so that output a destructor writes still appears.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn exit(status: c_int) -> ! {
    run_handlers();
    // SAFETY: this is `exit`, which runs the destructors once, after the
    // handlers.
    #[cfg(not(test))]
    unsafe {
        crate::start::run_fini();
    }
    crate::stdio::exit_flush();
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
    use std::sync::Mutex;

    static CALLS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    unsafe extern "C" fn record(arg: *mut c_void) {
        if let Ok(mut calls) = CALLS.lock() {
            calls.push(arg.addr());
        }
    }

    #[test]
    fn handlers_beyond_the_static_slots_run_newest_first_and_only_once() {
        for arg in 1..=40_usize {
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
        let calls = CALLS.lock().map(|calls| calls.clone()).unwrap_or_default();
        assert_eq!(calls, (1..=40).rev().collect::<Vec<_>>());
    }
}
