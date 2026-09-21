//! The initial thread's static TLS layout.
//!
//! The loader puts every `PT_TLS` image below the x86-64 thread pointer before
//! it runs constructors. That is enough for the initial-exec model: its one
//! relocation is an offset from `%fs`, and no resolver or per-thread dynamic
//! allocation is involved. `DTPMOD`, `DTPOFF` and `__tls_get_addr` are the
//! separate general-dynamic model and deliberately remain unsupported.

use core::mem::size_of;

use crate::report::Error;
use crate::scope::Scope;
use crate::sys::{self, nr};

/// `PROT_READ | PROT_WRITE`.
const PROT_READ_WRITE: usize = 0x1 | 0x2;
/// `MAP_PRIVATE | MAP_ANONYMOUS`.
const MAP_PRIVATE_ANONYMOUS: usize = 0x2 | 0x20;
/// glibc's alignment for the initial control block.
const TP_ALIGN: usize = 64;

/// The prefix program code expects at `%fs`: its self pointer, dynamic thread
/// vector and glibc-compatible `self` field. Ferrousli's C runtime replaces
/// it with its larger control block once its own entry code starts.
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
pub fn install(scope: &Scope) -> Result<(), Error> {
    if scope.tls_size() == 0 {
        return Ok(());
    }
    install_x86_64(scope)
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
        sys::syscall6(
            nr::MMAP,
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

    for index in 0..scope.len() {
        let Some(object) = scope.get(index) else {
            continue;
        };
        let Some(tls) = object.tls else {
            continue;
        };
        let destination = tp.cast::<u8>().wrapping_offset(tls.offset);
        // SAFETY: `layout_tls` gave every image a non-overlapping range in
        // this fresh, zeroed mapping; the source is its mapped `PT_TLS`
        // bytes, and `filesz <= memsz` was validated when it was read.
        unsafe {
            core::ptr::copy_nonoverlapping(tls.image as *const u8, destination, tls.filesz);
        }
    }
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
    let result = unsafe { sys::syscall2(nr::ARCH_PRCTL, ARCH_SET_FS, tp_address) };
    if result < 0 {
        return Err(Error::MalformedObject("could not set the initial thread pointer"));
    }
    Ok(())
}

#[cfg(not(target_arch = "x86_64"))]
fn install_x86_64(_: &Scope) -> Result<(), Error> {
    Err(Error::MalformedObject(
        "static TLS is not implemented for this architecture",
    ))
}
