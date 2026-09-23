//! The dynamic loader, when there is one: `ld-ferrousli`'s
//! `__ferrousli_loader`.
//!
//! Linked into a program, this library learns everything about the process
//! from the linker: `__init_array_start` bounds the program's constructors,
//! and the program's own `PT_TLS` is all the thread-local storage there is.
//! As `libferrousli.so` it knows neither -- those symbols are its own -- so
//! it asks the loader, which exports [`Interface`] for the purpose
//! (`ferrousli/ld/src/interface.rs`, whose layout this mirrors).
//!
//! The reference is weak. In a static program nothing defines the symbol and
//! [`interface`] is `None`, which is also how the rest of the library tells
//! which of the two it is in. Rust has no weak references on its stable
//! compiler, so the one place that takes the address is a few instructions of
//! assembly per architecture, reading the address from the GOT the loader
//! filled in.

use core::ffi::{c_char, c_int, c_void};

/// The revision of the interface this library reads. A loader reporting an
/// older one is treated as absent rather than read past its end.
const VERSION: usize = 1;

/// What the loader exports. The same layout as the loader's.
#[repr(C)]
#[derive(Debug)]
pub struct Interface {
    /// The loader's revision of this structure.
    pub version: usize,
    /// Bytes of static TLS below every thread pointer.
    pub tls_size: usize,
    /// The greatest alignment any object's `PT_TLS` asks for.
    pub tls_align: usize,
    /// Copies every object's initial TLS image below a new thread pointer.
    pub init_tls: unsafe extern "C" fn(tp: *mut u8),
    /// Runs the program's own `DT_INIT` and `DT_INIT_ARRAY` with its
    /// arguments, and makes the loader's exit callback finish it too.
    pub run_program_init: unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char),
    /// Revision 2: `dlopen`.
    pub dlopen: unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void,
    /// Revision 2: `dlsym`.
    pub dlsym: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    /// Revision 2: `dlclose`.
    pub dlclose: extern "C" fn(*mut c_void) -> c_int,
    /// Revision 2: `dlerror`.
    pub dlerror: extern "C" fn() -> *mut c_char,
    /// Revision 2: `dladdr`, with `Dl_info` as `link.rs` declares it.
    pub dladdr: unsafe extern "C" fn(*const c_void, *mut crate::link::DlInfo) -> c_int,
    /// Revision 2: `dl_iterate_phdr`.
    pub dl_iterate_phdr: unsafe extern "C" fn(Option<crate::link::Callback>, *mut c_void) -> c_int,
}

/// The revision that added the `dlfcn.h` calls.
const DLFCN: usize = 2;

/// The loader's interface if it has the `dlfcn.h` calls: `None` in a static
/// program, and with a loader older than revision 2.
#[must_use]
pub fn dlfcn() -> Option<&'static Interface> {
    interface().filter(|interface| interface.version >= DLFCN)
}

/// The loader's interface, or `None` in a statically linked program.
#[must_use]
pub fn interface() -> Option<&'static Interface> {
    #[cfg(test)]
    let at: *const Interface = core::ptr::null();
    // SAFETY: the function reads one GOT entry, which the loader filled in or
    // the linker left zero.
    #[cfg(not(test))]
    let at = unsafe { __ferrousli_loader_address() };
    // SAFETY: a non-null address is the loader's `__ferrousli_loader`, which
    // lives as long as the process and is not written after the program
    // starts.
    let interface = unsafe { at.as_ref() }?;
    (interface.version >= VERSION).then_some(interface)
}

#[cfg(not(test))]
unsafe extern "C" {
    /// The address of `__ferrousli_loader`, or null. Defined below.
    fn __ferrousli_loader_address() -> *const Interface;
}

#[cfg(all(not(test), target_arch = "x86_64"))]
core::arch::global_asm!(
    ".weak __ferrousli_loader",
    ".pushsection .text.__ferrousli_loader_address,\"ax\",@progbits",
    ".p2align 4",
    ".globl __ferrousli_loader_address",
    ".hidden __ferrousli_loader_address",
    ".type __ferrousli_loader_address, @function",
    "__ferrousli_loader_address:",
    "    mov rax, qword ptr [rip + __ferrousli_loader@GOTPCREL]",
    "    ret",
    ".size __ferrousli_loader_address, . - __ferrousli_loader_address",
    ".popsection",
);

#[cfg(all(not(test), target_arch = "aarch64"))]
core::arch::global_asm!(
    ".weak __ferrousli_loader",
    ".pushsection .text.__ferrousli_loader_address,\"ax\",%progbits",
    ".p2align 2",
    ".globl __ferrousli_loader_address",
    ".hidden __ferrousli_loader_address",
    ".type __ferrousli_loader_address, %function",
    "__ferrousli_loader_address:",
    "    adrp x0, :got:__ferrousli_loader",
    "    ldr x0, [x0, :got_lo12:__ferrousli_loader]",
    "    ret",
    ".size __ferrousli_loader_address, . - __ferrousli_loader_address",
    ".popsection",
);

// ARMv7-A has no PC-relative load of a GOT entry in one instruction: the GOT
// is found from the program counter, which reads 8 ahead in ARM state, and
// the entry at its offset in it.
#[cfg(all(not(test), target_arch = "arm"))]
core::arch::global_asm!(
    ".weak __ferrousli_loader",
    ".pushsection .text.__ferrousli_loader_address,\"ax\",%progbits",
    ".p2align 2",
    ".arm",
    ".globl __ferrousli_loader_address",
    ".hidden __ferrousli_loader_address",
    ".type __ferrousli_loader_address, %function",
    "__ferrousli_loader_address:",
    "    ldr r0, 2f",
    "1:  add r0, pc, r0",
    "    ldr r1, 3f",
    "    ldr r0, [r0, r1]",
    "    bx lr",
    "2:  .word _GLOBAL_OFFSET_TABLE_ - (1b + 8)",
    "3:  .word __ferrousli_loader(GOT)",
    ".size __ferrousli_loader_address, . - __ferrousli_loader_address",
    ".popsection",
);
