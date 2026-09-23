//! Thread-local storage: the initial thread's layout, and the calls code
//! makes to find a variable at run time.
//!
//! The loader puts every `PT_TLS` image below the x86-64 thread pointer, or
//! past the control block above it on AArch64 and ARMv7-A, before it runs
//! constructors. That is all the initial-exec model needs: its one
//! relocation is an offset from the thread pointer.
//!
//! # The general-dynamic models, on static TLS
//!
//! Code built with `-fPIC` for a shared library asks at run time instead:
//! `__tls_get_addr` with a module number and an offset (`DTPMOD`, `DTPOFF`),
//! or, with `-mtls-dialect=gnu2`, a TLS descriptor -- a function and its
//! argument -- which it calls. Every module this loader loads is loaded before
//! the program starts, so every module's block is in static TLS at an offset
//! from the thread pointer that is the same in every thread. The answer to
//! both calls is therefore that offset plus the variable's, found in
//! [`MODULE_OFFSETS`], with no dynamic thread vector and no allocation. A
//! module loaded later, by `dlopen`, will need the vector; there is no
//! `dlopen` yet.

use core::cell::UnsafeCell;

use crate::object::MAX_OBJECTS;
use crate::report::Error;
use crate::scope::Scope;
use crate::sys;

/// Entries in [`MODULE_OFFSETS`]: one per possible object, and module 0,
/// which no object is.
const MODULES: usize = MAX_OBJECTS + 1;

/// Each module's offset from the thread pointer, by module number: what
/// `__tls_get_addr` adds to a variable's offset inside its module's block.
/// Zero for a module without `PT_TLS`, which nothing asks about.
#[repr(transparent)]
struct Offsets(UnsafeCell<[isize; MODULES]>);

// SAFETY: written only by `publish_offsets`, by the only thread, before the
// program runs; read-only afterwards.
unsafe impl Sync for Offsets {}

/// The table `__tls_get_addr` reads, by symbol, from assembly.
static MODULE_OFFSETS: Offsets = Offsets(UnsafeCell::new([0; MODULES]));

/// Record every module's static TLS offset for `__tls_get_addr`.
///
/// # Safety
///
/// Called once, after [`Scope::layout_tls`], by the only thread, before the
/// program runs.
pub(crate) unsafe fn publish_offsets(scope: &Scope) {
    let table = MODULE_OFFSETS.0.get().cast::<isize>();
    for index in 0..scope.len().min(MAX_OBJECTS) {
        let Some(tls) = scope.get(index).and_then(|object| object.tls) else {
            continue;
        };
        // SAFETY: `index + 1` is below `MODULES`, and nothing reads the table
        // yet.
        unsafe { table.wrapping_add(index + 1).write(tls.offset) };
    }
}

/// The bytes of control block at the thread pointer in TLS variant I, before
/// the first block: glibc's `tcbhead_t`, a `dtv` pointer and a word the ABI
/// reserves.
#[cfg(target_arch = "aarch64")]
pub(crate) const TCB_SIZE: usize = 16;
/// As on AArch64, in two 32-bit words.
#[cfg(target_arch = "arm")]
pub(crate) const TCB_SIZE: usize = 8;

/// The calling thread's thread pointer: `%fs:0`, which holds itself.
#[cfg(target_arch = "x86_64")]
#[must_use]
pub(crate) fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reads `%fs:0`, which the loader or the C library set to the
    // thread pointer before any code that could call this ran.
    unsafe {
        core::arch::asm!(
            "mov {}, qword ptr fs:[0]",
            out(reg) tp,
            options(nostack, readonly, preserves_flags),
        );
    }
    tp
}

/// The calling thread's thread pointer: `tpidr_el0`.
#[cfg(target_arch = "aarch64")]
#[must_use]
pub(crate) fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reads the thread's own register.
    unsafe {
        core::arch::asm!(
            "mrs {}, tpidr_el0",
            out(reg) tp,
            options(nomem, nostack, preserves_flags),
        );
    }
    tp
}

/// The calling thread's thread pointer: the user read-only thread ID
/// register, `TPIDRURO`, which the kernel sets from `set_tls`.
#[cfg(target_arch = "arm")]
#[must_use]
pub(crate) fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reads a coprocessor register the thread may read.
    unsafe {
        core::arch::asm!(
            "mrc p15, 0, {}, c13, c0, 3",
            out(reg) tp,
            options(nomem, nostack, preserves_flags),
        );
    }
    tp
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
unsafe extern "C" {
    /// The function every TLS descriptor points at: defined below.
    fn __ferrousli_tlsdesc_static();
}

/// The function a `TLSDESC` relocation writes into its descriptor's first
/// word. Called with the descriptor in `rax` (`x0` on AArch64), it returns
/// there the second word, the variable's offset from the thread pointer, and
/// touches nothing else -- the descriptor ABI lets the caller keep every
/// other register live.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[must_use]
pub(crate) fn descriptor_function() -> usize {
    __ferrousli_tlsdesc_static as unsafe extern "C" fn() as usize
}

// `__tls_get_addr(&{module, offset})` and the descriptor function. Both in
// assembly because neither may use the stack: GCC has emitted calls to
// `__tls_get_addr` without aligning it, and the descriptor ABI allows no
// clobbers but `rax` and the flags. A module number past the table is a
// relocation this loader did not write, and stops the program.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".pushsection .text.__tls_get_addr,\"ax\",@progbits",
    ".p2align 4",
    ".globl __tls_get_addr",
    ".type __tls_get_addr, @function",
    "__tls_get_addr:",
    "    mov rax, qword ptr [rdi]",
    "    cmp rax, {modules}",
    "    jae 2f",
    "    lea rcx, [rip + {offsets}]",
    "    mov rax, qword ptr [rcx + 8*rax]",
    "    add rax, qword ptr [rdi + 8]",
    "    add rax, qword ptr fs:[0]",
    "    ret",
    "2:",
    "    ud2",
    ".size __tls_get_addr, . - __tls_get_addr",
    ".p2align 4",
    ".globl __ferrousli_tlsdesc_static",
    ".hidden __ferrousli_tlsdesc_static",
    ".type __ferrousli_tlsdesc_static, @function",
    "__ferrousli_tlsdesc_static:",
    "    mov rax, qword ptr [rax + 8]",
    "    ret",
    ".size __ferrousli_tlsdesc_static, . - __ferrousli_tlsdesc_static",
    ".popsection",
    modules = const MODULES,
    offsets = sym MODULE_OFFSETS,
);

// The same two on AArch64: the index in `x0`, the answer in `x0`, and `x1`
// and `x2` free, since `__tls_get_addr` is an ordinary call. The descriptor
// function may use only `x0`.
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".pushsection .text.__tls_get_addr,\"ax\",%progbits",
    ".p2align 4",
    ".globl __tls_get_addr",
    ".type __tls_get_addr, %function",
    "__tls_get_addr:",
    "    ldr x1, [x0]",
    "    cmp x1, #{modules}",
    "    b.hs 2f",
    "    adrp x2, {offsets}",
    "    add x2, x2, :lo12:{offsets}",
    "    ldr x1, [x2, x1, lsl #3]",
    "    ldr x2, [x0, #8]",
    "    add x1, x1, x2",
    "    mrs x2, tpidr_el0",
    "    add x0, x1, x2",
    "    ret",
    "2:",
    "    udf #0",
    ".size __tls_get_addr, . - __tls_get_addr",
    ".p2align 4",
    ".globl __ferrousli_tlsdesc_static",
    ".hidden __ferrousli_tlsdesc_static",
    ".type __ferrousli_tlsdesc_static, %function",
    "__ferrousli_tlsdesc_static:",
    "    ldr x0, [x0, #8]",
    "    ret",
    ".size __ferrousli_tlsdesc_static, . - __ferrousli_tlsdesc_static",
    ".popsection",
    modules = const MODULES,
    offsets = sym MODULE_OFFSETS,
);

// `__tls_get_addr` on ARMv7-A, in ARM state: the index in `r0`, the answer in
// `r0`, and `r1` to `r3` free. The table's address is its distance from the
// `add` that reads `pc`, eight bytes past itself. GCC's ARM code asks for
// general-dynamic variables this way rather than with descriptors.
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    ".pushsection .text.__tls_get_addr,\"ax\",%progbits",
    ".p2align 2",
    ".arm",
    ".globl __tls_get_addr",
    ".type __tls_get_addr, %function",
    "__tls_get_addr:",
    "    ldr r1, [r0]",
    "    cmp r1, #{modules}",
    "    bhs 2f",
    "    ldr r2, 3f",
    "4:",
    "    add r2, pc, r2",
    "    ldr r1, [r2, r1, lsl #2]",
    "    ldr r2, [r0, #4]",
    "    add r1, r1, r2",
    "    mrc p15, 0, r2, c13, c0, 3",
    "    add r0, r1, r2",
    "    bx lr",
    "2:",
    "    udf #0",
    "3:",
    "    .word {offsets} - (4b + 8)",
    ".size __tls_get_addr, . - __tls_get_addr",
    ".popsection",
    modules = const MODULES,
    offsets = sym MODULE_OFFSETS,
);

/// `PROT_READ | PROT_WRITE`.
const PROT_READ_WRITE: usize = 0x1 | 0x2;
/// `MAP_PRIVATE | MAP_ANONYMOUS`.
const MAP_PRIVATE_ANONYMOUS: usize = 0x2 | 0x20;
/// glibc's alignment for the initial control block.
const TP_ALIGN: usize = 64;

/// The prefix program code expects at `%fs`: its self pointer, dynamic thread
/// vector and glibc-compatible `self` field. Ferrousli's C runtime replaces
/// it with its larger control block once its own entry code starts.
#[cfg(target_arch = "x86_64")]
#[repr(C, align(64))]
struct Thread {
    tp: *mut Thread,
    dtv: *mut usize,
    this: *mut Thread,
}

/// Put the initial thread's static TLS images in fresh memory and select it
/// as the thread pointer.
///
/// A scope without `PT_TLS` needs no thread pointer; preserving the kernel's
/// zero value in that case avoids changing programs which do not use TLS.
///
/// # Errors
///
/// [`Error::MalformedObject`] if static TLS cannot fit in an address, the
/// kernel cannot provide the required storage, or this architecture has no
/// implementation yet.
pub(crate) fn install(scope: &Scope) -> Result<(), Error> {
    if scope.tls_size() == 0 {
        return Ok(());
    }
    #[cfg(target_arch = "x86_64")]
    return install_x86_64(scope);
    #[cfg(not(target_arch = "x86_64"))]
    install_upwards(scope)
}

/// [`install`] for variant I: a mapping holding the control block at the
/// thread pointer, zeroed, and every block past it.
///
/// A page below the thread pointer is zeroed memory too. ferrousli keeps
/// its own control block there, `errno` among it, and the library's
/// constructors, which run before its start builds one, may set `errno`.
#[cfg(not(target_arch = "x86_64"))]
fn install_upwards(scope: &Scope) -> Result<(), Error> {
    const TOO_LARGE: Error = Error::MalformedObject("TLS layout is too large");
    /// Room below the thread pointer.
    const BELOW: usize = 4096;
    let align = scope.tls_align().max(TP_ALIGN);
    let len = TCB_SIZE
        .checked_add(scope.tls_size())
        .and_then(|value| value.checked_add(align))
        .and_then(|value| value.checked_add(BELOW))
        .ok_or(TOO_LARGE)?;
    // SAFETY: this is a new private anonymous mapping, owned by the process.
    let mapped = unsafe {
        sys::mmap(
            0,
            len,
            PROT_READ_WRITE,
            MAP_PRIVATE_ANONYMOUS,
            usize::MAX,
            0,
        )
    };
    let base = usize::try_from(mapped)
        .map_err(|_| Error::MalformedObject("could not allocate initial TLS"))?;
    let tp = base
        .checked_add(BELOW + align - 1)
        .map(|value| value & !(align - 1))
        .ok_or(TOO_LARGE)?;
    // SAFETY: the mapping is fresh and zeroed, which is the control block's
    // `dtv` and reserved word, with every block's range after it.
    unsafe { copy_images(scope, tp as *mut u8) };
    set_thread_pointer(tp)
}

/// Select `tp` as the calling thread's thread pointer.
#[cfg(target_arch = "aarch64")]
fn set_thread_pointer(tp: usize) -> Result<(), Error> {
    // SAFETY: `tpidr_el0` is the thread's own register, and `tp` the live
    // control block just built.
    unsafe {
        core::arch::asm!("msr tpidr_el0, {}", in(reg) tp, options(nostack, preserves_flags));
    }
    Ok(())
}

/// Select `tp` as the calling thread's thread pointer: the kernel keeps it,
/// through ARM's private `set_tls` call.
#[cfg(target_arch = "arm")]
fn set_thread_pointer(tp: usize) -> Result<(), Error> {
    // SAFETY: `tp` names the live control block just built.
    let result = unsafe { sys::syscall2(sys::nr::ARM_SET_TLS, tp, 0) };
    if result < 0 {
        return Err(Error::MalformedObject(
            "could not set the initial thread pointer",
        ));
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn install_x86_64(scope: &Scope) -> Result<(), Error> {
    let align = scope.tls_align().max(TP_ALIGN);
    let len = scope
        .tls_size()
        .checked_add(align)
        .and_then(|value| value.checked_add(size_of::<Thread>()))
        .ok_or(Error::MalformedObject("TLS layout is too large"))?;
    // SAFETY: this is a new private anonymous mapping, owned by the process.
    let mapped = unsafe {
        sys::mmap(
            0,
            len,
            PROT_READ_WRITE,
            MAP_PRIVATE_ANONYMOUS,
            usize::MAX,
            0,
        )
    };
    let base = usize::try_from(mapped)
        .map_err(|_| Error::MalformedObject("could not allocate initial TLS"))?;
    let tp_address = base
        .checked_add(scope.tls_size())
        .and_then(|value| value.checked_add(align - 1))
        .map(|value| value & !(align - 1))
        .ok_or(Error::MalformedObject("TLS layout is too large"))?;
    let tp = tp_address as *mut Thread;

    // SAFETY: the mapping is fresh and zeroed, with `tls_size` bytes below
    // `tp`.
    unsafe { copy_images(scope, tp.cast()) };
    // SAFETY: `tp_address` is aligned and has room for this small control
    // block in the mapping; no code can observe it before `%fs` changes.
    unsafe {
        tp.write(Thread {
            tp,
            dtv: core::ptr::null_mut(),
            this: tp,
        });
    }
    // `ARCH_SET_FS` from `asm/prctl.h`.
    const ARCH_SET_FS: usize = 0x1002;
    // SAFETY: `tp` names the live, aligned control block just built above.
    let result = unsafe { sys::syscall2(sys::nr::ARCH_PRCTL, ARCH_SET_FS, tp_address) };
    if result < 0 {
        return Err(Error::MalformedObject(
            "could not set the initial thread pointer",
        ));
    }
    Ok(())
}

/// Copy every object's initial TLS image to its place relative to `tp`.
///
/// For the initial thread here, and for every later one through
/// [`crate::interface`], whose C library reserves the same layout.
///
/// # Safety
///
/// The [`Scope::tls_size`] bytes below `tp` on x86-64, or past the control
/// block above it on AArch64 and ARMv7-A, must be writable, zeroed and
/// otherwise unused, and every object in `scope` mapped.
pub(crate) unsafe fn copy_images(scope: &Scope, tp: *mut u8) {
    for index in 0..scope.len() {
        let Some(object) = scope.get(index) else {
            continue;
        };
        let Some(tls) = object.tls else {
            continue;
        };
        let destination = tp.wrapping_offset(tls.offset);
        // SAFETY: `layout_tls` gave every image a non-overlapping range
        // beside the thread pointer; the source is its mapped `PT_TLS` bytes,
        // and `filesz <= memsz` was validated when it was read.
        unsafe {
            core::ptr::copy_nonoverlapping(tls.image as *const u8, destination, tls.filesz);
        }
    }
}
