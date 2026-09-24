//! glibc's own names beyond any standard header that Chrome, systemd and
//! the C++ runtime link against: the program's name as
//! `program_invocation_name` and `__progname`, `__libc_stack_end`,
//! `__cxa_thread_atexit_impl`, `__register_atfork`, and `__sched_cpualloc`
//! with `__sched_cpufree`.
//!
//! The variables have two names at one address each, as glibc gives them,
//! so they are defined in assembly, as `environ` is (`stdlib.rs`), and the
//! library reaches them through its GOT: a program built against glibc may
//! hold its own copy of one, which is then the variable.

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use core::mem::size_of;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use crate::key::{pthread_getspecific, pthread_key_create, pthread_setspecific};
use crate::lock::SpinLock;
use crate::malloc::{free, malloc};

// `__progname_full` and `program_invocation_name`, `__progname` and
// `program_invocation_short_name`, and `__libc_stack_end`, in `.bss`.
#[cfg(all(not(test), target_pointer_width = "64"))]
core::arch::global_asm!(
    ".pushsection .bss.ferrousli_progname,\"aw\",%nobits",
    ".p2align 3",
    ".globl __progname_full",
    ".type __progname_full, %object",
    ".size __progname_full, 8",
    ".globl program_invocation_name",
    ".type program_invocation_name, %object",
    ".size program_invocation_name, 8",
    "__progname_full:",
    "program_invocation_name:",
    ".zero 8",
    ".globl __progname",
    ".type __progname, %object",
    ".size __progname, 8",
    ".globl program_invocation_short_name",
    ".type program_invocation_short_name, %object",
    ".size program_invocation_short_name, 8",
    "__progname:",
    "program_invocation_short_name:",
    ".zero 8",
    ".globl __libc_stack_end",
    ".type __libc_stack_end, %object",
    ".size __libc_stack_end, 8",
    "__libc_stack_end:",
    ".zero 8",
    ".popsection",
);

// The same with 4-byte pointers.
#[cfg(all(not(test), target_pointer_width = "32"))]
core::arch::global_asm!(
    ".pushsection .bss.ferrousli_progname,\"aw\",%nobits",
    ".p2align 2",
    ".globl __progname_full",
    ".type __progname_full, %object",
    ".size __progname_full, 4",
    ".globl program_invocation_name",
    ".type program_invocation_name, %object",
    ".size program_invocation_name, 4",
    "__progname_full:",
    "program_invocation_name:",
    ".zero 4",
    ".globl __progname",
    ".type __progname, %object",
    ".size __progname, 4",
    ".globl program_invocation_short_name",
    ".type program_invocation_short_name, %object",
    ".size program_invocation_short_name, 4",
    "__progname:",
    "program_invocation_short_name:",
    ".zero 4",
    ".globl __libc_stack_end",
    ".type __libc_stack_end, %object",
    ".size __libc_stack_end, 4",
    "__libc_stack_end:",
    ".zero 4",
    ".popsection",
);

#[cfg(not(test))]
unsafe extern "C" {
    /// `program_invocation_name`, defined above.
    #[link_name = "__progname_full"]
    static PROGNAME_FULL: AtomicPtr<c_char>;
    /// `program_invocation_short_name`, defined above.
    #[link_name = "__progname"]
    static PROGNAME: AtomicPtr<c_char>;
    /// `__libc_stack_end`, defined above.
    #[link_name = "__libc_stack_end"]
    static STACK_END: AtomicPtr<c_void>;
}

/// The variables, in unit tests, where the test binary's own C library
/// defines the C names.
#[cfg(test)]
static PROGNAME_FULL: AtomicPtr<c_char> = AtomicPtr::new(null_mut());
/// See [`PROGNAME_FULL`].
#[cfg(test)]
static PROGNAME: AtomicPtr<c_char> = AtomicPtr::new(null_mut());
/// See [`PROGNAME_FULL`].
#[cfg(test)]
static STACK_END: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

/// Records the program's name, `argv[0]`, and the start of its stack, as
/// glibc does before any of the program's code runs: `__progname_full` is
/// the name, `__progname` what follows its last `/`, and
/// `__libc_stack_end` the stack pointer `_start` was entered with.
///
/// # Safety
///
/// `argv0` must be null or a NUL-terminated string that lives as long as
/// the process.
pub unsafe fn init(argv0: *mut c_char, stack_end: *mut c_void) {
    #[allow(unused_unsafe, reason = "the variables are Rust statics in unit tests")]
    // SAFETY: the variable lives as long as the process, and every access to
    // it is atomic.
    let full = unsafe { &PROGNAME_FULL };
    #[allow(unused_unsafe, reason = "the variables are Rust statics in unit tests")]
    // SAFETY: as above.
    let short = unsafe { &PROGNAME };
    #[allow(unused_unsafe, reason = "the variables are Rust statics in unit tests")]
    // SAFETY: as above.
    let end = unsafe { &STACK_END };
    end.store(stack_end, Ordering::Relaxed);
    if argv0.is_null() {
        return;
    }
    let mut last = argv0;
    let mut p = argv0;
    // SAFETY: `argv0` is a NUL-terminated string, read up to its NUL.
    while unsafe { p.read() } != 0 {
        // SAFETY: as above.
        if unsafe { p.read() } == b'/' as c_char {
            last = p.wrapping_add(1);
        }
        p = p.wrapping_add(1);
    }
    full.store(argv0, Ordering::Relaxed);
    short.store(last, Ordering::Relaxed);
}

/// A destructor `__cxa_thread_atexit_impl` registered, and the one before.
struct ThreadDtor {
    /// The destructor.
    dtor: unsafe extern "C" fn(*mut c_void),
    /// Its object.
    obj: *mut c_void,
    /// The destructor registered before it, run after it.
    next: *mut ThreadDtor,
}

/// The key whose value on each thread is its list of destructors, plus one;
/// 0 until the first registration.
static DTOR_KEY: AtomicU32 = AtomicU32::new(0);
/// Guards creating [`DTOR_KEY`].
static DTOR_KEY_LOCK: SpinLock = SpinLock::new();

/// Runs the destructors on the list `head`, newest first, freeing each node
/// before its destructor runs.
///
/// # Safety
///
/// `head` must be a list [`__cxa_thread_atexit_impl`] built, which nothing
/// else runs.
unsafe extern "C" fn run_list(head: *mut c_void) {
    let mut node = head.cast::<ThreadDtor>();
    while !node.is_null() {
        // SAFETY: a node this module allocated and nothing else holds.
        let ThreadDtor { dtor, obj, next } = unsafe { node.read() };
        // SAFETY: as above; it is not used again.
        unsafe { free(node.cast()) };
        // SAFETY: the registration vouched for the destructor and object.
        unsafe { dtor(obj) };
        node = next;
    }
}

/// The key, created on first use, or `None` if none is left.
fn dtor_key() -> Option<c_uint> {
    let key = DTOR_KEY.load(Ordering::Acquire);
    if key != 0 {
        return Some(key - 1);
    }
    let _guard = DTOR_KEY_LOCK.lock();
    let key = DTOR_KEY.load(Ordering::Acquire);
    if key != 0 {
        return Some(key - 1);
    }
    let mut created = 0;
    // SAFETY: `created` is a live local, and `run_list` is the destructor.
    if unsafe { pthread_key_create(&raw mut created, Some(run_list)) } != 0 {
        return None;
    }
    DTOR_KEY.store(created + 1, Ordering::Release);
    Some(created)
}

/// Registers `dtor(obj)` to run when the calling thread exits, before its
/// thread-specific data's destructors, or for the thread that calls `exit`,
/// before the `atexit` handlers: glibc's `__cxa_thread_atexit_impl`, which
/// the C++ runtime calls for each `thread_local` object with a destructor.
/// Destructors run newest first. Returns 0, or -1 if there is no memory.
///
/// glibc also keeps the object's shared library loaded until then, which
/// `dso` names; this loader never unloads one, so that is already so.
///
/// # Safety
///
/// `dtor` must be sound to call with `obj` on the calling thread when it
/// exits.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __cxa_thread_atexit_impl(
    dtor: Option<unsafe extern "C" fn(*mut c_void)>,
    obj: *mut c_void,
    _dso: *mut c_void,
) -> c_int {
    let (Some(dtor), Some(key)) = (dtor, dtor_key()) else {
        return -1;
    };
    let node = malloc(size_of::<ThreadDtor>()).cast::<ThreadDtor>();
    if node.is_null() {
        return -1;
    }
    let next = pthread_getspecific(key).cast::<ThreadDtor>();
    // SAFETY: fresh memory, large and aligned enough for a node.
    unsafe { node.write(ThreadDtor { dtor, obj, next }) };
    if pthread_setspecific(key, node.cast()) != 0 {
        // SAFETY: the node was never published.
        unsafe { free(node.cast()) };
        return -1;
    }
    0
}

/// Runs the calling thread's `thread_local` destructors, for `exit`, which
/// runs them before its handlers, as glibc's does.
pub fn run_thread_dtors_at_exit() {
    let key = DTOR_KEY.load(Ordering::Acquire);
    if key == 0 {
        return;
    }
    let head = pthread_getspecific(key - 1);
    let _ = pthread_setspecific(key - 1, null_mut());
    // SAFETY: the list was this thread's, now taken from it.
    unsafe { run_list(head) };
}

/// glibc's `pthread_atfork` with the calling library's handle, which its
/// headers call it through.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __register_atfork(
    prepare: Option<extern "C" fn()>,
    parent: Option<extern "C" fn()>,
    child: Option<extern "C" fn()>,
    _dso: *mut c_void,
) -> c_int {
    crate::pthread::pthread_atfork(prepare, parent, child)
}

/// A CPU set for `count` CPUs, from `malloc`, for glibc's `CPU_ALLOC`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __sched_cpualloc(count: usize) -> *mut c_ulong {
    let bits = c_ulong::BITS as usize;
    malloc(count.div_ceil(bits) * size_of::<c_ulong>()).cast()
}

/// Frees a set from [`__sched_cpualloc`], for glibc's `CPU_FREE`.
///
/// # Safety
///
/// `set` must be null or from `__sched_cpualloc`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __sched_cpufree(set: *mut c_ulong) {
    // SAFETY: the caller's contract is `free`'s.
    unsafe { free(set.cast()) }
}
