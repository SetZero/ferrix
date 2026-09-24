//! `__libc_start_main`: from the kernel's stack to `main` and back to `exit`.
//!
//! `crt1.o` calls this with glibc's argument list, not musl's, so that a
//! program carrying glibc's own `crt1.o` starts the same way.
//!
//! Before any of the program's code runs, this records the environment and
//! the auxiliary vector and sets up the thread pointer. Code built with the
//! stack protector reads the canary through the thread pointer in its first
//! instructions, and that includes constructors.

use core::ffi::{CStr, c_char, c_int, c_void};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::exit::exit;
use crate::stdlib::environ;
use crate::{auxv, thread};

/// A C `main`, which may take fewer arguments than this. The x86-64 calling
/// convention makes passing unused ones harmless.
type Main = unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char) -> c_int;

/// A destructor from `.fini_array`, which is called with no arguments.
pub type Hook = Option<unsafe extern "C" fn()>;

/// A constructor from `.preinit_array` or `.init_array`.
///
/// glibc and musl both call these with the program's `argc`, `argv` and
/// `envp`, and a constructor compiled for Linux may read them. This library
/// called them with nothing, which no program has yet been found to notice,
/// but which is the contract being wrong rather than merely unused: glibc's
/// `call_init` passes all three, and musl's `__libc_start_main` does too.
///
/// A constructor that takes no arguments is still safe to call through this
/// type, for the reason [`Main`] may be called with three: on x86-64 the extra
/// arguments sit in registers the callee does not read.
pub type InitHook = Option<unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char)>;

unsafe extern "C" {
    // Bounds the linker defines around the arrays in a static program. Only
    // their addresses are used. The `end` symbols are one past the last entry
    // and are never read.
    static __preinit_array_start: InitHook;
    static __preinit_array_end: InitHook;
    static __init_array_start: InitHook;
    static __init_array_end: InitHook;
    static __fini_array_start: Hook;
    static __fini_array_end: Hook;
}

/// The program's name as it was started, `argv[0]`: a GNU extension musl has
/// too, which Mesa asks for to know which program it is driving.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static program_invocation_name: AtomicPtr<c_char> = AtomicPtr::new(null_mut());

/// The same name after its last `/`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static program_invocation_short_name: AtomicPtr<c_char> = AtomicPtr::new(null_mut());

/// Record `name`, the program's `argv[0]` or null, as both of the names
/// above, before a constructor can read them.
///
/// # Safety
///
/// `name` is null or a C string that lives as long as the process, as the
/// kernel's arguments on the initial stack do.
unsafe fn name_program(name: *mut c_char) {
    program_invocation_name.store(name, Ordering::Relaxed);
    let mut short = name;
    if !name.is_null() {
        // SAFETY: the caller's: a C string.
        let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
        if let Some(slash) = bytes.iter().rposition(|&byte| byte == b'/') {
            short = name.wrapping_add(slash + 1);
        }
    }
    program_invocation_short_name.store(short, Ordering::Relaxed);
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
    init: InitHook,
    fini: Hook,
    rtld_fini: Hook,
    _stack_end: *mut c_void,
) -> ! {
    let count = usize::try_from(argc).unwrap_or(0);
    let envp = argv.wrapping_add(count + 1);
    environ().store(envp, Ordering::Relaxed);
    // SAFETY: `argv` holds `argc` pointers and a null, and nothing else runs.
    unsafe { name_program(if count > 0 { argv.read() } else { null_mut() }) };

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

    // The dynamic loader supplied this through `rdx`, and `crt1.o` passed it
    // here. Register it before constructors and `main` can add their own
    // handlers, so they run first and the loaded libraries finish afterwards.
    if let Some(rtld_fini) = rtld_fini {
        let _ = crate::exit::atexit(Some(rtld_fini));
    }

    // As `libferrousli.so`, the bounds below are this library's own arrays,
    // not the program's; only the loader knows where the program's are.
    if let Some(loader) = crate::loader::interface() {
        // A `crt1.o` from before glibc 2.34 passes the program's own
        // `__libc_csu_init` and `__libc_csu_fini`, which walk its arrays; a
        // later one passes null and leaves it to the C library, which here
        // leaves it to the loader, whose exit callback then finishes the
        // program too.
        if let Some(init) = init {
            if let Some(fini) = fini {
                let _ = crate::exit::atexit(Some(fini));
            }
            // SAFETY: the program's own initialiser, called with its
            // arguments, now that the thread pointer is set.
            unsafe { init(argc, argv, envp) };
        } else {
            // SAFETY: the loader runs the program's constructors, which may
            // use this library, and it is started.
            unsafe { (loader.run_program_init)(argc, argv, envp) };
        }
        // SAFETY: `main` is the program's, called with the kernel's arguments.
        let status = unsafe { main(argc, argv, envp) };
        exit(status)
    }

    // Put the program's array after the loader callback. Handlers registered
    // by constructors and `main` are newer and therefore run first; this
    // walker then finishes the program before the loader finishes its
    // dependencies, the reverse of their initialisation order.
    let _ = crate::exit::atexit(Some(run_fini));

    // SAFETY: in a static program the linker puts function pointers between
    // each pair of bounds, and the thread pointer they may use is set.
    unsafe {
        run(
            &raw const __preinit_array_start,
            &raw const __preinit_array_end,
            argc,
            argv,
            envp,
        );
    }
    // SAFETY: as above.
    unsafe {
        run(
            &raw const __init_array_start,
            &raw const __init_array_end,
            argc,
            argv,
            envp,
        );
    }

    // SAFETY: `main` is the program's, called with the kernel's arguments.
    let status = unsafe { main(argc, argv, envp) };
    exit(status)
}

/// Calls each constructor in `start..end`, in order, with the program's
/// arguments and environment.
///
/// # Safety
///
/// The range must hold constructors that are sound to call now, and `argv` and
/// `envp` must be the program's.
unsafe fn run(
    start: *const InitHook,
    end: *const InitHook,
    argc: c_int,
    argv: *mut *mut c_char,
    envp: *mut *mut c_char,
) {
    let mut at = start;
    while at < end {
        // SAFETY: the caller vouches that `at` is inside the array, and the
        // array sections are pointer-aligned.
        let hook = unsafe { at.read() };
        if let Some(hook) = hook {
            // SAFETY: the caller vouches for calling each constructor, and for
            // the arguments being the program's.
            unsafe { hook(argc, argv, envp) };
        }
        at = at.wrapping_add(1);
    }
}

/// Runs the program's `.fini_array`, last entry first.
///
/// # Safety
///
/// Registered by [`__libc_start_main`] and called once by `exit`.
pub unsafe extern "C" fn run_fini() {
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

/// The glibc version this library claims to be, as `gnu/libc-version.h`
/// declares it: a NUL-terminated string such as `2.39`.
///
/// It is a claim about which glibc interfaces exist here, not a version of
/// this library. A program asks so that it can decide whether a newer
/// interface is available; Rust's `std` is one, which is how this came to be
/// needed. 2.39 is the newest glibc whose additions this library has —
/// `__isoc23_strtol` and its relatives from 2.38, `fchmodat2` from 2.39.
/// **Raise it when an interface from a later glibc is added, and not before:
/// a number that is too high makes callers take a path that is not here.**
const LIBC_VERSION: &CStr = c"2.39";

/// The release this library claims to be, as glibc reports it. glibc says
/// `stable` for a released version, and so does this.
const LIBC_RELEASE: &CStr = c"stable";

/// glibc's `gnu_get_libc_version`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gnu_get_libc_version() -> *const c_char {
    LIBC_VERSION.as_ptr()
}

/// glibc's `gnu_get_libc_release`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gnu_get_libc_release() -> *const c_char {
    LIBC_RELEASE.as_ptr()
}
