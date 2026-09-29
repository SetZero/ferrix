//! `pthread.h`'s thread-specific data, `pthread_key_create` and its
//! relatives, and `pthread_once`.
//!
//! Adapted from musl (MIT). There are `PTHREAD_KEYS_MAX` keys. Each thread
//! has an array of that many values, at the top of its mapping, or in a static
//! for the main thread. A key's slot in the process-wide table holds its
//! destructor, or a sentinel for a key with none, and is null while the key is
//! free. A read-write lock guards the table: creating and deleting keys write,
//! running destructors reads.
//!
//! When a thread exits, each non-null value whose key has a destructor is set
//! to null and passed to the destructor. A destructor may set values again,
//! so this repeats up to `PTHREAD_DESTRUCTOR_ITERATIONS` times.

use core::ffi::{c_int, c_uint, c_void};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicI32, AtomicPtr, AtomicUsize, Ordering};

use crate::cancel::{self, Cleanup};
use crate::rwlock::Rwlock;
use crate::{errno, futex, pthread, thread};

/// `PTHREAD_KEYS_MAX`, from `limits.h`.
pub const KEYS_MAX: usize = 128;
/// `PTHREAD_DESTRUCTOR_ITERATIONS`, from `limits.h`.
const DESTRUCTOR_ITERATIONS: usize = 4;

/// A destructor.
type Destructor = unsafe extern "C" fn(*mut c_void);

/// The main thread's values.
static MAIN_TSD: [AtomicPtr<c_void>; KEYS_MAX] = [const { AtomicPtr::new(null_mut()) }; KEYS_MAX];
/// Each key's destructor as a data pointer, [`no_destructor`] for none, or
/// null while the key is free.
static KEYS: [AtomicPtr<()>; KEYS_MAX] = [const { AtomicPtr::new(null_mut()) }; KEYS_MAX];
/// Guards [`KEYS`].
pub static KEY_LOCK: Rwlock = Rwlock::new();
/// Where the search for a free key starts.
static NEXT_KEY: AtomicUsize = AtomicUsize::new(0);

/// The main thread's value array, for its control block.
pub fn main_tsd() -> *mut AtomicPtr<c_void> {
    (&raw const MAIN_TSD).cast_mut().cast()
}

/// Marks a key with no destructor. Never called.
unsafe extern "C" fn no_destructor(_value: *mut c_void) {}

/// The sentinel stored for a key with no destructor.
fn sentinel() -> *mut () {
    no_destructor as Destructor as *mut ()
}

/// Slot `key` of the value array at `tsd`.
///
/// # Safety
///
/// `tsd` must be a thread's value array, which lives as long as the thread,
/// and `key` below [`KEYS_MAX`].
unsafe fn slot<'a>(tsd: *mut AtomicPtr<c_void>, key: usize) -> &'a AtomicPtr<c_void> {
    // SAFETY: the caller vouches for the array and the index.
    unsafe { &*tsd.wrapping_add(key) }
}

/// Creates a key with destructor `dtor`, which may be null, and stores it in
/// `*key`. Fails with `EAGAIN` if every key is taken.
///
/// # Safety
///
/// `key` must be valid for a write of a `pthread_key_t`, and `dtor` null or
/// sound to call with any value the program stores under the key.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_key_create(key: *mut c_uint, dtor: Option<Destructor>) -> c_int {
    let dtor = dtor.map_or_else(sentinel, |dtor| dtor as *mut ());
    // SAFETY: there is no time limit.
    let _ = unsafe { KEY_LOCK.write(core::ptr::null()) };
    let start = NEXT_KEY.load(Ordering::SeqCst);
    let mut j = start;
    loop {
        if let Some(entry) = KEYS.get(j)
            && entry.load(Ordering::SeqCst).is_null()
        {
            entry.store(dtor, Ordering::SeqCst);
            NEXT_KEY.store(j, Ordering::SeqCst);
            let _ = KEY_LOCK.unlock();
            // SAFETY: the caller vouches for `key`. `j` is below 128.
            unsafe { key.write(j as c_uint) };
            return 0;
        }
        j = (j + 1) % KEYS_MAX;
        if j == start {
            let _ = KEY_LOCK.unlock();
            return errno::EAGAIN;
        }
    }
}

/// Deletes `key`, forgetting every thread's value for it without running its
/// destructor. Fails with `EINVAL` for a key that was never created.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_key_delete(key: c_uint) -> c_int {
    let key = key as usize;
    let Some(entry) = KEYS.get(key) else {
        return errno::EINVAL;
    };
    let mask = pthread::block_app_signals();
    // SAFETY: there is no time limit.
    let _ = unsafe { KEY_LOCK.write(core::ptr::null()) };
    let r = if entry.load(Ordering::SeqCst).is_null() {
        errno::EINVAL
    } else {
        pthread::list_lock();
        pthread::for_each_thread(|t| {
            // SAFETY: threads on the list are live while the lock is held.
            let tsd = unsafe { thread::state(t) }.tsd.load(Ordering::SeqCst);
            if !tsd.is_null() {
                // SAFETY: each thread's array has `KEYS_MAX` slots.
                unsafe { slot(tsd, key) }.store(null_mut(), Ordering::SeqCst);
            }
        });
        pthread::list_unlock();
        entry.store(null_mut(), Ordering::SeqCst);
        0
    };
    let _ = KEY_LOCK.unlock();
    pthread::restore_signals(mask);
    r
}

/// The calling thread's value for `key`, or null for a key out of range.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_getspecific(key: c_uint) -> *mut c_void {
    let key = key as usize;
    if key >= KEYS_MAX {
        return null_mut();
    }
    let tsd = thread::me().tsd.load(Ordering::Relaxed);
    // SAFETY: the calling thread's array has `KEYS_MAX` slots.
    unsafe { slot(tsd, key) }.load(Ordering::Relaxed)
}

/// Sets the calling thread's value for `key`. Fails with `EINVAL` for a key
/// out of range.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_setspecific(key: c_uint, value: *const c_void) -> c_int {
    let key = key as usize;
    if key >= KEYS_MAX {
        return errno::EINVAL;
    }
    let state = thread::me();
    let tsd = state.tsd.load(Ordering::Relaxed);
    // SAFETY: the calling thread's array has `KEYS_MAX` slots.
    let entry = unsafe { slot(tsd, key) };
    if entry.load(Ordering::Relaxed).cast_const() != value {
        entry.store(value.cast_mut(), Ordering::Relaxed);
        state.tsd_used.store(1, Ordering::Relaxed);
    }
    0
}

/// Runs the calling thread's destructors, up to
/// `PTHREAD_DESTRUCTOR_ITERATIONS` rounds while they leave values set.
pub fn run_destructors() {
    let state = thread::me();
    let tsd = state.tsd.load(Ordering::SeqCst);
    if tsd.is_null() {
        return;
    }
    let mut round = 0;
    while state.tsd_used.load(Ordering::SeqCst) != 0 && round < DESTRUCTOR_ITERATIONS {
        // SAFETY: there is no time limit.
        let _ = unsafe { KEY_LOCK.read(core::ptr::null()) };
        state.tsd_used.store(0, Ordering::SeqCst);
        for (key, entry) in KEYS.iter().enumerate() {
            // SAFETY: the array has `KEYS_MAX` slots.
            let value = unsafe { slot(tsd, key) }.swap(null_mut(), Ordering::SeqCst);
            let dtor = entry.load(Ordering::SeqCst);
            if !value.is_null() && !dtor.is_null() && dtor != sentinel() {
                // SAFETY: only `pthread_key_create` stores destructors.
                let dtor = unsafe { core::mem::transmute::<*mut (), Destructor>(dtor) };
                // A destructor may create or delete keys.
                let _ = KEY_LOCK.unlock();
                // SAFETY: the program registered it for values under the key.
                unsafe { dtor(value) };
                // SAFETY: as above.
                let _ = unsafe { KEY_LOCK.read(core::ptr::null()) };
            }
        }
        let _ = KEY_LOCK.unlock();
        round += 1;
    }
}

/// Puts a `pthread_once` control back to its initial state when its routine
/// is cancelled, and wakes every waiter, whose waiting is forgotten with it.
unsafe extern "C" fn undo_once(control: *mut c_void) {
    // SAFETY: `pthread_once` registered its control, which outlives the call.
    let control = unsafe { AtomicI32::from_ptr(control.cast()) };
    if control.swap(0, Ordering::SeqCst) == 3 {
        futex::wake(control, -1, true);
    }
}

/// Runs `init` once in the life of the process for `*control`, however many
/// threads call this, and waits while another thread runs it.
///
/// The control is 0 before, 1 while `init` runs, 3 while it runs and others
/// wait, and 2 after. If `init` is cancelled, the control goes back to 0 and a
/// waiting thread runs it instead, as POSIX requires.
///
/// # Safety
///
/// `control` must be a `pthread_once_t` initialised to `PTHREAD_ONCE_INIT`,
/// and `init` sound to call.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_once(
    control: *mut c_int,
    init: Option<unsafe extern "C" fn()>,
) -> c_int {
    // SAFETY: the caller vouches for `control`, and every access is atomic.
    let control = unsafe { AtomicI32::from_ptr(control) };
    if control.load(Ordering::SeqCst) == 2 {
        return 0;
    }
    let Some(init) = init else {
        return errno::EINVAL;
    };
    loop {
        match futex::cas(control, 0, 1) {
            0 => {
                let mut cb = Cleanup {
                    func: None,
                    arg: null_mut(),
                    next: null_mut(),
                };
                // SAFETY: `cb` lives until the matching pop, and the handler
                // resets this control, which outlives both.
                unsafe {
                    cancel::_pthread_cleanup_push(
                        &raw mut cb,
                        Some(undo_once),
                        control.as_ptr().cast(),
                    );
                }
                // SAFETY: the caller vouches for `init`.
                unsafe { init() };
                // SAFETY: `cb` is the handler pushed above.
                unsafe { cancel::_pthread_cleanup_pop(&raw mut cb, 0) };
                if control.swap(2, Ordering::SeqCst) == 3 {
                    futex::wake(control, -1, true);
                }
                return 0;
            }
            1 => {
                let _ = futex::cas(control, 1, 3);
                futex::wait_counted(control, None, 3, true);
            }
            3 => futex::wait_counted(control, None, 3, true),
            _ => return 0,
        }
    }
}
