//! `__libc_start_main`: from the kernel's stack to `main` and back to `exit`.
//!
//! `crt1.o` calls this with glibc's argument list, not musl's, so that a
//! program carrying glibc's own `crt1.o` starts the same way.
//!
//! Before any of the program's code runs, this records the environment and
//! the auxiliary vector and sets up the thread pointer. Code built with the
//! stack protector reads the canary through the thread pointer in its first
//! instructions, and that includes constructors.

use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::Ordering;

use crate::exit::exit;
use crate::stdlib::environ;
use crate::{auxv, thread};

/// A C `main`, which may take fewer arguments than this. The x86-64 calling
/// convention makes passing unused ones harmless.
type Main = unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char) -> c_int;

/// A constructor or destructor from `.init_array` or `.fini_array`.
pub type Hook = Option<unsafe extern "C" fn()>;

unsafe extern "C" {
    // Bounds the linker defines around the arrays in a static program. Only
    // their addresses are used. The `end` symbols are one past the last entry
    // and are never read.
    static __preinit_array_start: Hook;
    static __preinit_array_end: Hook;
    static __init_array_start: Hook;
    static __init_array_end: Hook;
    static __fini_array_start: Hook;
    static __fini_array_end: Hook;
}

/// Sets up the process, runs `main`, and exits with what it returns.
///
/// # Safety
///
/// Called once, by `_start`, with the `argc` and `argv` the kernel placed on
/// the stack. The environment must follow `argv`'s terminating null, and the
/// auxiliary vector the environment's.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __libc_start_main(
    main: Main,
    argc: c_int,
    argv: *mut *mut c_char,
    _init: Hook,
    _fini: Hook,
    _rtld_fini: Hook,
    _stack_end: *mut c_void,
) -> ! {
    let count = usize::try_from(argc).unwrap_or(0);
    let envp = argv.wrapping_add(count + 1);
    environ.store(envp, Ordering::Relaxed);

    let mut at = envp;
    // SAFETY: the environment ends in a null, and `at` has not passed it.
    while !unsafe { at.read() }.is_null() {
        at = at.wrapping_add(1);
    }
    // SAFETY: the kernel places the auxiliary vector after the environment's
    // null, on the initial stack, which lives as long as the process.
    unsafe { auxv::init(at.wrapping_add(1).cast::<usize>().cast_const()) };
    // SAFETY: nothing has read the thread pointer, and this runs once.
    unsafe { thread::init_main() };

    // SAFETY: in a static program the linker puts function pointers between
    // each pair of bounds, and the thread pointer they may use is set.
    unsafe {
        run(
            &raw const __preinit_array_start,
            &raw const __preinit_array_end,
        );
    }
    // SAFETY: as above.
    unsafe {
        run(&raw const __init_array_start, &raw const __init_array_end);
    }

    // SAFETY: `main` is the program's, called with the kernel's arguments.
    let status = unsafe { main(argc, argv, envp) };
    exit(status)
}

/// Calls each hook in `start..end`, in order.
///
/// # Safety
///
/// The range must hold hooks that are sound to call now.
unsafe fn run(start: *const Hook, end: *const Hook) {
    let mut at = start;
    while at < end {
        // SAFETY: the caller vouches that `at` is inside the array, and the
        // array sections are pointer-aligned.
        let hook = unsafe { at.read() };
        if let Some(hook) = hook {
            // SAFETY: the caller vouches for calling each hook.
            unsafe { hook() };
        }
        at = at.wrapping_add(1);
    }
}

/// Runs the program's `.fini_array`, last entry first.
///
/// # Safety
///
/// Called once, by `exit`.
pub unsafe fn run_fini() {
    let start = &raw const __fini_array_start;
    let mut at = &raw const __fini_array_end;
    while at > start {
        at = at.wrapping_sub(1);
        // SAFETY: `at` is inside the array the linker bounded, and the
        // section is pointer-aligned.
        let hook = unsafe { at.read() };
        if let Some(hook) = hook {
            // SAFETY: the caller runs the destructors once, at exit.
            unsafe { hook() };
        }
    }
}
