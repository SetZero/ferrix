//! What the loader tells the C library: `__ferrousli_loader`.
//!
//! A statically linked program's C library knows everything about the
//! process from the linker: `__init_array_start` bounds the program's
//! constructors, and `PT_TLS` in its own headers is the only thread-local
//! storage there is. Linked as `libferrousli.so` it knows neither. Its
//! `__init_array_start` is its own, and every library the loader mapped may
//! bring a `PT_TLS` of its own, placed by the loader where only the loader
//! knows.
//!
//! So the loader exports one symbol, and the library reaches it through a
//! weak reference. In a static program nothing defines it and the reference
//! is null, which is also how the library tells which of the two it is in.
//! glibc's equivalent is `_rtld_global` and `_rtld_global_ro`, which are
//! glibc's own business and far larger; this is ferrousli's, and holds only
//! what ferrousli asks.
//!
//! # The program's initialisers are the library's to ask for
//!
//! The loader runs every *library's* initialisers before it enters the
//! program, and never the program's own. That is glibc's division too: a
//! program's constructors may use its C library, which is not started until
//! `__libc_start_main` runs. A C library linked into the program runs them
//! from the linker's bounds; `libferrousli.so` asks here, through
//! [`Interface::run_program_init`], and from then on [`crate::start`]'s
//! `dl_fini` finishes the program as well as its libraries.
//!
//! # And `dlfcn.h`
//!
//! Revision 2 carries `dlopen` and the rest ([`crate::dl`]): as
//! `libc.so.6`, the C library's own functions of those names forward here.
//!
//! # And the auxiliary vector
//!
//! Revision 3 carries the vector the kernel gave the process. A library's
//! constructors run before the program's `__libc_start_main` has recorded it,
//! and some ask: compiler-builtins' choice of AArch64's LSE atomics reads
//! `AT_HWCAP` from one. `getauxval` answers them from here until the C
//! library has its own copy.

use core::ffi::{c_char, c_int, c_void};

use crate::dl;

/// The revision of [`Interface`] this loader fills in. A library built
/// against a later one reads `version` before anything past it. Revision 2
/// added the `dlfcn.h` calls, revision 3 the auxiliary vector.
const VERSION: usize = 3;

/// What the loader exports to the C library.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct Interface {
    /// [`VERSION`].
    pub(crate) version: usize,
    /// Bytes of static TLS below every thread pointer on x86-64, rounded to
    /// `tls_align`; on AArch64 and ARMv7-A, the bytes past the control block
    /// above it.
    pub(crate) tls_size: usize,
    /// The greatest alignment any `PT_TLS` asks for.
    pub(crate) tls_align: usize,
    /// Copy every object's initial TLS image into a new thread's blocks,
    /// beside the thread pointer given.
    pub(crate) init_tls: unsafe extern "C" fn(tp: *mut u8),
    /// Run the program's own `DT_INIT` and `DT_INIT_ARRAY`, with its
    /// arguments, and have the exit callback finish it too.
    pub(crate) run_program_init: unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char),
    /// `dlopen`.
    pub(crate) dlopen: unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void,
    /// `dlsym`.
    pub(crate) dlsym: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    /// `dlclose`.
    pub(crate) dlclose: extern "C" fn(*mut c_void) -> c_int,
    /// `dlerror`.
    pub(crate) dlerror: extern "C" fn() -> *mut c_char,
    /// `dladdr`.
    pub(crate) dladdr: unsafe extern "C" fn(*const c_void, *mut dl::DlInfo) -> c_int,
    /// `dl_iterate_phdr`.
    pub(crate) dl_iterate_phdr: unsafe extern "C" fn(
        Option<unsafe extern "C" fn(*mut dl::DlPhdrInfo, usize, *mut c_void) -> c_int>,
        *mut c_void,
    ) -> c_int,
    /// Revision 3: the auxiliary vector on the entry stack, ending in
    /// `AT_NULL`, or null before [`publish`].
    pub(crate) auxv: *const usize,
}

/// The interface, filled in once the scope is complete.
///
/// A `static mut` because the C library reads it as plain memory through its
/// GOT; it is written once, by the only thread, before the program runs.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals, reason = "a C symbol, named as one")]
pub(crate) static mut __ferrousli_loader: Interface = Interface {
    version: VERSION,
    tls_size: 0,
    tls_align: 1,
    init_tls,
    run_program_init,
    dlopen: dl::dlopen,
    dlsym: dl::dlsym,
    dlclose: dl::dlclose,
    dlerror: dl::dlerror,
    dladdr: dl::dladdr,
    dl_iterate_phdr: dl::dl_iterate_phdr,
    auxv: core::ptr::null(),
};

/// Publish the static TLS layout the scope settled on, and the auxiliary
/// vector, before any library's initialiser runs.
///
/// # Safety
///
/// Called once, before the program runs, by the only thread. `auxv` must be
/// the entry stack's vector, which stays in place for the life of the
/// process.
pub(crate) unsafe fn publish(tls_size: usize, tls_align: usize, auxv: *const usize) {
    let interface = &raw mut __ferrousli_loader;
    // SAFETY: the caller promises nothing else is reading it yet.
    unsafe { (*interface).tls_size = tls_size };
    // SAFETY: as above.
    unsafe { (*interface).tls_align = tls_align };
    // SAFETY: as above.
    unsafe { (*interface).auxv = auxv };
}

/// [`Interface::init_tls`].
unsafe extern "C" fn init_tls(tp: *mut u8) {
    // SAFETY: the caller hands over a fresh thread pointer with the reserved
    // space below it, and the lock keeps `dlopen` from writing the scope
    // while it is read.
    dl::with_scope(|scope| unsafe { crate::tls::copy_images(scope, tp) });
}

/// [`Interface::run_program_init`].
unsafe extern "C" fn run_program_init(argc: c_int, argv: *mut *mut c_char, envp: *mut *mut c_char) {
    let Some(program) = dl::with_scope(|scope| scope.get(0).copied()) else {
        return;
    };
    // SAFETY: the program is mapped and relocated, and its C library has
    // started, which is what its constructors may rely on. `dl_fini` finishes
    // it from here on, since this records it as initialised.
    unsafe { crate::run_object_init(&program, argc, argv, envp) };
}
