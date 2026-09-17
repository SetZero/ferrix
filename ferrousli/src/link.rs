//! `link.h`'s `dl_iterate_phdr` and the whole of `dlfcn.h`, for a static
//! program, which is the only object loaded.
//!
//! An unwinder finds the unwind tables for an address by walking the loaded
//! objects' program headers for the `PT_GNU_EH_FRAME` segment that covers it;
//! libunwind, and so every C++ exception, does exactly that. A static program
//! has one object, itself, whose headers the kernel names in the auxiliary
//! vector. Its load bias is zero for a program linked at a fixed address and
//! `AT_PHDR` less the `PT_PHDR` segment's address for a position-independent
//! one, as `thread.rs` computes it for TLS.

use core::ffi::{CStr, c_char, c_int, c_ulonglong, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::{null, null_mut, with_exposed_provenance};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{auxv, thread};

/// `Elf64_Phdr`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

const _: () = assert!(size_of::<Phdr>() == 56);

/// A loadable segment.
const PT_LOAD: u32 = 1;
/// The segment holding the program headers.
const PT_PHDR: u32 = 6;

/// `struct dl_phdr_info`, as `include/link.h` declares it; glibc's is the
/// same.
#[repr(C)]
#[derive(Debug)]
pub struct DlPhdrInfo {
    /// The object's load bias.
    pub dlpi_addr: u64,
    /// Its name; the empty string for the program itself.
    pub dlpi_name: *const c_char,
    /// Its program headers.
    pub dlpi_phdr: *const c_void,
    /// How many there are.
    pub dlpi_phnum: u16,
    /// Objects loaded since the program started, counting the program.
    pub dlpi_adds: c_ulonglong,
    /// Objects unloaded since the program started.
    pub dlpi_subs: c_ulonglong,
    /// The TLS module id: 1 for a program with a TLS segment, else 0.
    pub dlpi_tls_modid: usize,
    /// The calling thread's TLS block for the module, or null.
    pub dlpi_tls_data: *mut c_void,
}

const _: () = assert!(size_of::<DlPhdrInfo>() == 64);
const _: () = assert!(offset_of!(DlPhdrInfo, dlpi_phnum) == 24);
const _: () = assert!(offset_of!(DlPhdrInfo, dlpi_adds) == 32);
const _: () = assert!(offset_of!(DlPhdrInfo, dlpi_tls_data) == 56);

/// `Dl_info`, as `include/dlfcn.h` declares it.
#[repr(C)]
#[derive(Debug)]
pub struct DlInfo {
    /// The object's path name; empty for the program itself.
    pub dli_fname: *const c_char,
    /// The object's load address.
    pub dli_fbase: *mut c_void,
    /// The nearest symbol's name, or null.
    pub dli_sname: *const c_char,
    /// The nearest symbol's address, or null.
    pub dli_saddr: *mut c_void,
}

const _: () = assert!(size_of::<DlInfo>() == 32);

/// The program's headers, from the auxiliary vector: where they are, how
/// many, and the load bias.
fn program_headers() -> Option<(*const Phdr, usize, usize)> {
    let phdr = auxv::get(auxv::AT_PHDR)?;
    let phent = auxv::get(auxv::AT_PHENT)?;
    let phnum = auxv::get(auxv::AT_PHNUM)?;
    if phent != size_of::<Phdr>() {
        return None;
    }
    let mut bias = 0_usize;
    let mut i = 0;
    while i < phnum {
        // SAFETY: the kernel mapped `phnum` headers at `AT_PHDR`.
        let header = unsafe { header(phdr, i) };
        if header.p_type == PT_PHDR {
            bias = phdr.wrapping_sub(header.p_vaddr as usize);
        }
        i += 1;
    }
    Some((with_exposed_provenance(phdr), phnum, bias))
}

/// The `i`th program header at `phdr`.
///
/// # Safety
///
/// At least `i + 1` headers must be mapped at `phdr`.
unsafe fn header(phdr: usize, i: usize) -> Phdr {
    let at = with_exposed_provenance::<Phdr>(phdr.wrapping_add(i.wrapping_mul(size_of::<Phdr>())));
    // SAFETY: the caller vouches for the header.
    unsafe { at.read_unaligned() }
}

/// The type of `dl_iterate_phdr`'s callback.
pub type Callback = unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> c_int;

/// Calls `callback` once for each loaded object, which in a static program is
/// the program, with `data`, and returns what the last call returned. Returns
/// 0 without calling it if the kernel gave no program headers.
///
/// # Safety
///
/// `callback` must be safe to call with a `DlPhdrInfo` it may read and `data`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dl_iterate_phdr(callback: Option<Callback>, data: *mut c_void) -> c_int {
    let (Some(callback), Some((phdr, phnum, bias))) = (callback, program_headers()) else {
        return 0;
    };
    let tls = thread::tls_block();
    let mut info = DlPhdrInfo {
        dlpi_addr: bias as u64,
        dlpi_name: c"".as_ptr(),
        dlpi_phdr: phdr.cast(),
        dlpi_phnum: u16::try_from(phnum).unwrap_or(u16::MAX),
        dlpi_adds: 1,
        dlpi_subs: 0,
        dlpi_tls_modid: usize::from(tls.is_some()),
        dlpi_tls_data: tls.unwrap_or(null_mut()),
    };
    // SAFETY: the caller vouches for the callback; `info` lives across it.
    unsafe { callback(&raw mut info, size_of::<DlPhdrInfo>(), data) }
}

/// Describes the object holding `address`: in a static program, the program
/// when a loadable segment covers the address, with no symbol, since the
/// program carries no dynamic symbol table to name one from. Returns nonzero
/// when `info` was filled, and 0 for an address outside the program.
///
/// # Safety
///
/// `info` must be valid to write a `Dl_info` to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int {
    let Some((phdr, phnum, bias)) = program_headers() else {
        return 0;
    };
    let wanted = address.addr();
    let mut i = 0;
    while i < phnum {
        // SAFETY: `program_headers` counted `phnum` headers at `phdr`.
        let header = unsafe { header(phdr.addr(), i) };
        let start = bias.wrapping_add(header.p_vaddr as usize);
        if header.p_type == PT_LOAD
            && wanted >= start
            && wanted - start < usize::try_from(header.p_memsz).unwrap_or(0)
        {
            // SAFETY: the caller vouches for `info`.
            unsafe {
                info.write(DlInfo {
                    dli_fname: c"".as_ptr(),
                    dli_fbase: with_exposed_provenance::<c_void>(bias).cast_mut(),
                    dli_sname: null(),
                    dli_saddr: null_mut(),
                });
            }
            return 1;
        }
        i += 1;
    }
    0
}

/// Loads a shared object: never, in a static program. Returns null, and
/// [`dlerror`] then says why, as musl's static `dlopen` does.
///
/// # Safety
///
/// `path` must be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dlopen(_path: *const c_char, _flags: c_int) -> *mut c_void {
    fail();
    null_mut()
}

/// Looks a symbol up in a loaded object. A static program has loaded none, so
/// this finds nothing and returns null, as musl's static `dlsym` does.
///
/// Rust's `std` calls this to find functions it can do without, and takes null
/// as "the C library has not got it"; that is how this came to be needed.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dlsym(_handle: *mut c_void, _name: *const c_char) -> *mut c_void {
    fail();
    null_mut()
}

/// Closes a handle from [`dlopen`], which never gave one out. Returns -1.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dlclose(_handle: *mut c_void) -> c_int {
    fail();
    -1
}

/// The message for the last `dlfcn.h` call that failed, or null if none has
/// since this was last called. Every call here fails the same way.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dlerror() -> *mut c_char {
    if FAILED.swap(false, Ordering::Relaxed) {
        MESSAGE.as_ptr().cast_mut()
    } else {
        null_mut()
    }
}

/// Whether a `dlfcn.h` call has failed since [`dlerror`] last reported one.
static FAILED: AtomicBool = AtomicBool::new(false);

/// What [`dlerror`] says. musl's static build says the same.
const MESSAGE: &CStr = c"Dynamic loading not supported";

/// Records that a `dlfcn.h` call failed, for [`dlerror`] to report.
fn fail() {
    FAILED.store(true, Ordering::Relaxed);
}

/// Describes the loaded object holding `address` for an unwinder, as glibc
/// 2.35 and later do. Always answers "not found", and returns -1.
///
/// The caller is `_Unwind_Find_FDE` in libgcc, which calls this first and
/// falls back to [`dl_iterate_phdr`] — which this library does implement, and
/// which finds the program's unwind tables — when it is told nothing. So the
/// answer here is not a refusal to unwind; it only costs the fast path.
///
/// It is written this way on purpose. Filling `*result` needs glibc's `struct
/// dl_find_object`, and there is no header in `include/` to take that layout
/// from: musl has no such structure, and glibc's header is LGPL and must not
/// be copied. Not touching the structure at all needs no layout. `docs` has
/// the row for doing this properly, which wants the layout written down from
/// the ABI first.
///
/// # Safety
///
/// None: neither pointer is read or written.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn _dl_find_object(_address: *mut c_void, _result: *mut c_void) -> c_int {
    -1
}
