//! The thread control block, and a static program's thread-local storage.
//!
//! On x86-64 `%fs` holds the thread pointer. Compiled code reads fixed offsets
//! from it without calling the library:
//!
//! * `%fs:0` holds the thread pointer itself. Initial-exec and local-exec TLS
//!   load it to find variables below it.
//! * `%fs:0x28` holds the stack protector's canary, compiled into every
//!   function `-fstack-protector` guards.
//! * `%fs:0x70` holds the stack limit that `-fsplit-stack` prologues compare
//!   against.
//!
//! Those offsets come from glibc's `tcbhead_t`. [`Thread`] keeps glibc's layout
//! for its first 0x80 bytes, so that programs built for either C library find
//! what they expect. Ferrousli's own fields follow.
//!
//! # Where TLS goes
//!
//! x86-64 uses TLS variant II: the program's TLS block ends where the control
//! block begins. The linker resolved every local-exec reference as a negative
//! offset from the thread pointer, with the block starting
//! `round_up(p_memsz, p_align)` below it. The block holds the `PT_TLS`
//! segment's first `p_filesz` bytes, and zeros after them.

use core::cell::UnsafeCell;
use core::ffi::c_int;
use core::mem::{offset_of, size_of};
use core::ptr::{null, null_mut, with_exposed_provenance, with_exposed_provenance_mut};

use crate::auxv;
use crate::errno;
use crate::string;
use crate::syscall::{self, nr};

/// A thread's control block. The thread pointer points at it.
#[repr(C)]
#[derive(Debug)]
pub struct Thread {
    /// 0x00: the thread pointer itself.
    tp: *mut Thread,
    /// 0x08: the dynamic thread vector. Null until dynamic linking.
    dtv: *mut usize,
    /// 0x10: glibc's `self`.
    this: *mut Thread,
    /// 0x18: glibc's `multiple_threads`, `gscope_flag` and `sysinfo`.
    glibc_0x18: [usize; 2],
    /// 0x28: the stack protector's canary.
    stack_guard: usize,
    /// 0x30: glibc's pointer guard, which mangles saved code pointers.
    pointer_guard: usize,
    /// 0x38: glibc's `vgetcpu` cache, CET feature bits and transactional
    /// memory slots.
    glibc_0x38: [usize; 7],
    /// 0x70: the split-stack limit. Zero, since no stack here is split.
    split_stack_limit: usize,
    /// 0x78: glibc's `ssp_base`.
    glibc_0x78: usize,
    /// 0x80: this thread's `errno`.
    errno: c_int,
    /// 0x84: the kernel's id for this thread.
    tid: c_int,
}

const _: () = assert!(offset_of!(Thread, stack_guard) == 0x28);
const _: () = assert!(offset_of!(Thread, split_stack_limit) == 0x70);
const _: () = assert!(offset_of!(Thread, errno) == 0x80);

/// The least alignment of a thread pointer. glibc aligns its control block to
/// 64 bytes so that the loader can keep vector register state there.
const TP_ALIGN: usize = 64;

/// `arch_prctl`'s request to set the `%fs` base.
const ARCH_SET_FS: usize = 0x1002;

/// `PROT_READ | PROT_WRITE`.
const PROT_READ_WRITE: usize = 0x1 | 0x2;
/// `MAP_PRIVATE | MAP_ANONYMOUS`.
const MAP_PRIVATE_ANONYMOUS: usize = 0x02 | 0x20;

/// The start of a TLS block, below the thread pointer.
fn tls_offset(memsz: usize, align: usize) -> Option<usize> {
    let align = align.max(1);
    if !align.is_power_of_two() {
        return None;
    }
    memsz.checked_next_multiple_of(align)
}

/// How far a thread pointer must be aligned.
fn tp_align(align: usize) -> usize {
    align.max(TP_ALIGN)
}

/// Bytes to reserve for a thread whose TLS segment has this size and
/// alignment. That is the TLS block, up to one thread pointer alignment of
/// padding, and the control block.
pub fn reservation(memsz: usize, align: usize) -> Option<usize> {
    tls_offset(memsz, align)?
        .checked_add(tp_align(align))?
        .checked_add(size_of::<Thread>())
}

/// Where the thread pointer and the start of the TLS block go, inside `len`
/// bytes at `base`. `None` if they do not fit or the alignment is not a power
/// of two.
pub fn place(base: usize, len: usize, memsz: usize, align: usize) -> Option<(usize, usize)> {
    let offset = tls_offset(memsz, align)?;
    let tp = base
        .checked_add(offset)?
        .checked_next_multiple_of(tp_align(align))?;
    if tp.checked_add(size_of::<Thread>())? > base.checked_add(len)? {
        return None;
    }
    Some((tp, tp - offset))
}

/// The main thread's memory, when the program's TLS is small enough to share
/// it.
#[repr(C, align(64))]
struct Builtin(UnsafeCell<[u8; BUILTIN_LEN]>);

/// The size of [`Builtin`].
const BUILTIN_LEN: usize = 1024;

// SAFETY: only `init_main` touches the memory, once, before any other thread
// can exist.
unsafe impl Sync for Builtin {}

static BUILTIN: Builtin = Builtin(UnsafeCell::new([0; BUILTIN_LEN]));

/// A program header, as `Elf64_Phdr`.
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

/// The header that describes the program headers themselves.
const PT_PHDR: u32 = 6;
/// The header that describes the TLS image.
const PT_TLS: u32 = 7;

/// A program's TLS image.
#[derive(Debug)]
struct Tls {
    image: *const u8,
    filesz: usize,
    memsz: usize,
    align: usize,
}

/// The program's TLS image, read from its headers through the auxiliary
/// vector. A program without one gets an empty image.
fn program_tls() -> Tls {
    let empty = Tls {
        image: null(),
        filesz: 0,
        memsz: 0,
        align: 1,
    };
    let (Some(phdr), Some(phent), Some(phnum)) = (
        auxv::get(auxv::AT_PHDR),
        auxv::get(auxv::AT_PHENT),
        auxv::get(auxv::AT_PHNUM),
    ) else {
        return empty;
    };
    if phent < size_of::<Phdr>() {
        return empty;
    }

    // Where the program was loaded, for a position-independent one. A
    // program linked at a fixed address has a `PT_PHDR` at the address it
    // was loaded at, so this is zero.
    let mut base = 0_usize;
    let mut tls = None;
    let mut i = 0;
    while i < phnum {
        let at = with_exposed_provenance::<Phdr>(phdr.wrapping_add(i.wrapping_mul(phent)));
        // SAFETY: `AT_PHDR`, `AT_PHNUM` and `AT_PHENT` describe the program's
        // headers, which the kernel mapped.
        let header = unsafe { at.read_unaligned() };
        match header.p_type {
            PT_PHDR => base = phdr.wrapping_sub(header.p_vaddr as usize),
            PT_TLS => tls = Some(header),
            _ => {}
        }
        i += 1;
    }

    match tls {
        Some(header) => Tls {
            image: with_exposed_provenance(base.wrapping_add(header.p_vaddr as usize)),
            filesz: header.p_filesz as usize,
            memsz: header.p_memsz as usize,
            align: header.p_align as usize,
        },
        None => empty,
    }
}

/// The canary and the pointer guard, from the kernel's random bytes. The
/// canary's low byte is zero, so a string function that runs off the end of a
/// buffer stops before reading it, and cannot write it back unchanged.
fn guards() -> (usize, usize) {
    let Some(address) = auxv::get(auxv::AT_RANDOM) else {
        return (0x00ff_0a0d_0000_0000, 0);
    };
    // SAFETY: `AT_RANDOM` names sixteen bytes on the initial stack.
    let bytes = unsafe { with_exposed_provenance::<[u8; 16]>(address).read_unaligned() };
    let canary = bytes
        .first_chunk::<8>()
        .map_or(0, |chunk| usize::from_ne_bytes(*chunk));
    let pointer_guard = bytes
        .last_chunk::<8>()
        .map_or(0, |chunk| usize::from_ne_bytes(*chunk));
    (canary & !0xff, pointer_guard)
}

/// Fresh zeroed memory from the kernel, or a trap.
fn map(len: usize) -> *mut u8 {
    // SAFETY: a new anonymous private mapping aliases nothing. The file
    // descriptor is -1 as `mmap` requires for anonymous memory.
    let ret = unsafe {
        syscall::syscall6(
            nr::MMAP,
            0,
            len,
            PROT_READ_WRITE,
            MAP_PRIVATE_ANONYMOUS,
            usize::MAX,
            0,
        )
    };
    match errno::decode(ret) {
        Ok(address) => with_exposed_provenance_mut(address),
        Err(_) => syscall::trap(),
    }
}

/// Sets up the main thread: its TLS block, its control block, the thread
/// pointer and the canary. A program that cannot have a thread pointer cannot
/// run, so every failure traps.
///
/// # Safety
///
/// Called once, before anything reads the thread pointer, after
/// [`auxv::init`].
pub unsafe fn init_main() {
    let tls = program_tls();
    let Some(len) = reservation(tls.memsz, tls.align) else {
        syscall::trap()
    };
    let base = if len <= BUILTIN_LEN {
        BUILTIN.0.get().cast::<u8>()
    } else {
        map(len)
    };
    let Some((tp_address, block_address)) = place(base.addr(), len, tls.memsz, tls.align) else {
        syscall::trap()
    };
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "`place` aligned the thread pointer to at least 64 bytes"
    )]
    let tp = base.wrapping_add(tp_address - base.addr()).cast::<Thread>();
    let block = base.wrapping_add(block_address - base.addr());

    if tls.filesz > 0 {
        // SAFETY: the image is `filesz` mapped bytes, and the block has room
        // for `memsz`, which is at least `filesz`, in memory nothing else uses.
        let _ = unsafe { string::memcpy(block.cast(), tls.image.cast(), tls.filesz) };
    }

    let (stack_guard, pointer_guard) = guards();
    // SAFETY: `gettid` takes no arguments.
    let tid = unsafe { syscall::syscall0(nr::GETTID) } as c_int;
    // SAFETY: `place` put the control block inside the reservation, aligned,
    // and nothing else refers to that memory.
    unsafe {
        tp.write(Thread {
            tp,
            dtv: null_mut(),
            this: tp,
            glibc_0x18: [0; 2],
            stack_guard,
            pointer_guard,
            glibc_0x38: [0; 7],
            split_stack_limit: 0,
            glibc_0x78: 0,
            errno: 0,
            tid,
        });
    }

    // SAFETY: the new `%fs` base is a control block that lives for the rest
    // of the process.
    let ret = unsafe { syscall::syscall2(nr::ARCH_PRCTL, ARCH_SET_FS, tp.expose_provenance()) };
    if ret != 0 {
        syscall::trap();
    }
}

/// The calling thread's control block.
///
/// Before [`init_main`] the thread pointer is zero, and this faults.
pub fn current() -> *mut Thread {
    let tp: usize;
    // SAFETY: reading `%fs:0` reads memory only. After `init_main` it holds
    // the thread pointer, and before it the read faults rather than
    // returning garbage.
    unsafe {
        core::arch::asm!(
            "mov {}, qword ptr fs:[0]",
            out(reg) tp,
            options(nostack, readonly, preserves_flags),
        );
    }
    with_exposed_provenance_mut(tp)
}

/// The calling thread's `errno`.
pub fn errno_location() -> *mut c_int {
    current()
        .wrapping_byte_add(offset_of!(Thread, errno))
        .cast::<c_int>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tls_block_ends_where_the_aligned_thread_pointer_begins() {
        assert_eq!(reservation(5, 1), Some(5 + 64 + size_of::<Thread>()));
        // Five bytes, unaligned: the block starts five bytes below a thread
        // pointer on a 64-byte boundary.
        assert_eq!(place(0x1000, 1024, 5, 1), Some((0x1040, 0x103b)));
        // Twenty bytes aligned to 16 occupy 32 below the thread pointer.
        assert_eq!(place(0x1000, 1024, 20, 16), Some((0x1040, 0x1020)));
        // An alignment above 64 aligns the thread pointer too.
        assert_eq!(place(0x1000, 1024, 100, 128), Some((0x1080, 0x1000)));
        // A zero alignment means none.
        assert_eq!(place(0x1000, 1024, 5, 0), Some((0x1040, 0x103b)));
    }

    #[test]
    fn a_layout_that_does_not_fit_is_refused() {
        assert_eq!(place(0x1000, 100, 5, 1), None);
        assert_eq!(place(0x1000, 1024, 5, 3), None);
        assert_eq!(reservation(usize::MAX, 16), None);
    }
}
