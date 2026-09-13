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
//! what they expect. Ferrousli's own fields follow: `errno`, the kernel's
//! thread id, the locale `uselocale` set, and the [`State`] the thread
//! functions share.
//!
//! # Where TLS goes
//!
//! x86-64 uses TLS variant II: the program's TLS block ends where the control
//! block begins. The linker resolved every local-exec reference as a negative
//! offset from the thread pointer, with the block starting
//! `round_up(p_memsz, p_align)` below it. The block holds the `PT_TLS`
//! segment's first `p_filesz` bytes, and zeros after them.
//!
//! Every thread gets the same layout. [`init_main`] records the program's TLS
//! image for [`new_control_block`], which `pthread_create` calls on memory it
//! mapped for the new thread.
//!
//! # Sharing a control block
//!
//! Other threads read and write a thread's [`State`] while it runs: a joiner
//! waits on its detach state, `pthread_cancel` sets its cancel flag. So every
//! field of `State` is an atomic, and [`state`] hands out a shared reference to
//! it alone. Nothing makes a reference to a whole `Thread`, whose `errno` its
//! own thread writes without one.

use core::cell::UnsafeCell;
use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::{null, null_mut, with_exposed_provenance, with_exposed_provenance_mut};
use core::sync::atomic::{AtomicI32, AtomicIsize, AtomicPtr, AtomicU8, AtomicUsize, Ordering};

use crate::cancel::Cleanup;
use crate::locale::Locale;
use crate::lock::SpinLock;
use crate::syscall::{self, nr};
use crate::{arch, auxv, errno, string};

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
    /// 0x84: the kernel's id for this thread. Zero once the thread has begun
    /// to exit, so that nothing signals a reused id.
    tid: AtomicI32,
    /// 0x88: the locale `uselocale` gave this thread, or null while it follows
    /// the global locale. A new thread starts with null: the global locale.
    locale: *mut Locale,
    /// 0x90: what the thread functions share about this thread.
    state: State,
}

/// The part of a control block other threads may touch while the thread runs.
/// Every field is atomic.
#[repr(C)]
#[derive(Debug)]
pub struct State {
    /// Whether the thread is joinable, detached, exiting or gone: one of
    /// `pthread`'s `DT_` values. Joiners sleep on it.
    pub detach_state: AtomicI32,
    /// Nonzero once `pthread_cancel` has asked the thread to end.
    pub cancel: AtomicI32,
    /// `PTHREAD_CANCEL_ENABLE`, `PTHREAD_CANCEL_DISABLE`, or the library's
    /// masked state, in which a cancellation point reports `ECANCELED`.
    pub cancel_disable: AtomicU8,
    /// Nonzero for asynchronous cancellation.
    pub cancel_async: AtomicU8,
    /// Nonzero if the thread may have set a thread-specific value.
    pub tsd_used: AtomicU8,
    /// Held while anything uses the thread's id to act on it, and while the
    /// thread clears the id on exit.
    pub kill_lock: SpinLock,
    /// The previous thread in the list of running threads.
    pub prev: AtomicPtr<Thread>,
    /// The next thread in the list of running threads.
    pub next: AtomicPtr<Thread>,
    /// The start of the mapping holding the thread's stack, TLS and control
    /// block, or zero for the main thread.
    pub map_base: AtomicUsize,
    /// The length of that mapping.
    pub map_size: AtomicUsize,
    /// The top of the thread's stack, or zero for the main thread.
    pub stack: AtomicUsize,
    /// The usable size of the stack below `stack`.
    pub stack_size: AtomicUsize,
    /// The size of the inaccessible guard below the stack.
    pub guard_size: AtomicUsize,
    /// What the thread returned or passed to `pthread_exit`.
    pub result: AtomicPtr<c_void>,
    /// The innermost cleanup handler `pthread_cleanup_push` registered.
    pub cancel_buf: AtomicPtr<Cleanup>,
    /// The thread's `PTHREAD_KEYS_MAX` thread-specific values.
    pub tsd: AtomicPtr<AtomicPtr<c_void>>,
    /// The kernel's `struct robust_list_head`, from `linux/futex.h`: the
    /// robust mutexes the thread holds, as a list through each mutex's `next`
    /// field. It points at itself when empty.
    pub robust_head: AtomicPtr<c_void>,
    /// The offset from a mutex's `next` field to its lock word.
    pub robust_off: AtomicIsize,
    /// A mutex the thread is in the middle of linking or unlinking.
    pub robust_pending: AtomicPtr<c_void>,
}

const _: () = assert!(offset_of!(Thread, stack_guard) == 0x28);
const _: () = assert!(offset_of!(Thread, pointer_guard) == 0x30);
const _: () = assert!(offset_of!(Thread, split_stack_limit) == 0x70);
const _: () = assert!(offset_of!(Thread, errno) == 0x80);
const _: () = assert!(offset_of!(Thread, tid) == 0x84);
const _: () = assert!(offset_of!(Thread, locale) == 0x88);
const _: () = assert!(offset_of!(Thread, state) == 0x90);
// `struct robust_list_head` is three words: the list, the offset and the
// pending entry, at 0, 8 and 16.
const _: () = assert!(offset_of!(State, robust_off) - offset_of!(State, robust_head) == 8);
const _: () = assert!(offset_of!(State, robust_pending) - offset_of!(State, robust_head) == 16);

impl State {
    /// The state of a thread that is joinable and has done nothing yet.
    const fn new() -> Self {
        Self {
            detach_state: AtomicI32::new(0),
            cancel: AtomicI32::new(0),
            cancel_disable: AtomicU8::new(0),
            cancel_async: AtomicU8::new(0),
            tsd_used: AtomicU8::new(0),
            kill_lock: SpinLock::new(),
            prev: AtomicPtr::new(null_mut()),
            next: AtomicPtr::new(null_mut()),
            map_base: AtomicUsize::new(0),
            map_size: AtomicUsize::new(0),
            stack: AtomicUsize::new(0),
            stack_size: AtomicUsize::new(0),
            guard_size: AtomicUsize::new(0),
            result: AtomicPtr::new(null_mut()),
            cancel_buf: AtomicPtr::new(null_mut()),
            tsd: AtomicPtr::new(null_mut()),
            robust_head: AtomicPtr::new(null_mut()),
            robust_off: AtomicIsize::new(0),
            robust_pending: AtomicPtr::new(null_mut()),
        }
    }

    /// The address of the robust list head, which is what the kernel is given
    /// and what an empty list points back at.
    pub fn robust_head_address(&self) -> *mut c_void {
        (&raw const self.robust_head).cast_mut().cast()
    }
}

/// The least alignment of a thread pointer. glibc aligns its control block to
/// 64 bytes so that the loader can keep vector register state there.
const TP_ALIGN: usize = 64;

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

/// The program's TLS image's address, recorded by [`init_main`] for new
/// threads.
static TLS_IMAGE: AtomicUsize = AtomicUsize::new(0);
/// How many bytes of the image are initialised.
static TLS_FILESZ: AtomicUsize = AtomicUsize::new(0);
/// The size of a TLS block.
static TLS_MEMSZ: AtomicUsize = AtomicUsize::new(0);
/// The alignment of a TLS block.
static TLS_ALIGN: AtomicUsize = AtomicUsize::new(1);

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

/// Bytes a new thread needs for its TLS block and control block.
pub fn tls_reservation() -> Option<usize> {
    reservation(
        TLS_MEMSZ.load(Ordering::Relaxed),
        TLS_ALIGN.load(Ordering::Relaxed),
    )
}

/// Builds a control block in `len` bytes at `base`: places the thread pointer
/// and the TLS block, copies the program's TLS image, and writes a fresh
/// control block with the given guards.
///
/// # Safety
///
/// `base..base + len` must be zeroed memory nothing else uses, and
/// [`TLS_IMAGE`] and its companions must describe the program's image.
unsafe fn build(
    base: *mut u8,
    len: usize,
    stack_guard: usize,
    pointer_guard: usize,
) -> Option<*mut Thread> {
    let memsz = TLS_MEMSZ.load(Ordering::Relaxed);
    let align = TLS_ALIGN.load(Ordering::Relaxed);
    let (tp_address, block_address) = place(base.addr(), len, memsz, align)?;
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "`place` aligned the thread pointer to at least 64 bytes"
    )]
    let tp = base.wrapping_add(tp_address - base.addr()).cast::<Thread>();
    let block = base.wrapping_add(block_address - base.addr());

    let filesz = TLS_FILESZ.load(Ordering::Relaxed);
    if filesz > 0 {
        let image = with_exposed_provenance::<c_void>(TLS_IMAGE.load(Ordering::Relaxed));
        // SAFETY: the image is `filesz` mapped bytes, and the block has room
        // for `memsz`, which is at least `filesz`, in memory nothing else uses.
        let _ = unsafe { string::memcpy(block.cast(), image, filesz) };
    }

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
            tid: AtomicI32::new(0),
            locale: null_mut(),
            state: State::new(),
        });
    }
    // SAFETY: the control block was just written.
    let state = unsafe { state(tp) };
    state
        .robust_head
        .store(state.robust_head_address(), Ordering::Relaxed);
    state.prev.store(tp, Ordering::Relaxed);
    state.next.store(tp, Ordering::Relaxed);
    Some(tp)
}

/// Builds a new thread's control block in `len` bytes at `base`, which must
/// be at least [`tls_reservation`] bytes, with the calling thread's canary and
/// pointer guard. `None` if it does not fit.
///
/// # Safety
///
/// `base..base + len` must be zeroed memory nothing else uses, and
/// [`init_main`] must have run.
pub unsafe fn new_control_block(base: *mut u8, len: usize) -> Option<*mut Thread> {
    let me = current();
    // SAFETY: after `init_main` the thread pointer is a control block, and
    // its guards never change.
    let stack_guard = unsafe { (*me).stack_guard };
    // SAFETY: as above.
    let pointer_guard = unsafe { (*me).pointer_guard };
    // SAFETY: the caller vouches for the memory, and `init_main` recorded the
    // image.
    unsafe { build(base, len, stack_guard, pointer_guard) }
}

/// Sets up the main thread: its TLS block, its control block, the thread
/// pointer and the canary. A program that cannot have a thread pointer cannot
/// run, so every failure traps.
///
/// The kernel is told to clear the thread list lock when the main thread
/// ends, as every thread's `clone` tells it, so that the lock's holder is
/// always a live thread.
///
/// # Safety
///
/// Called once, before anything reads the thread pointer, after
/// [`auxv::init`].
pub unsafe fn init_main() {
    let tls = program_tls();
    TLS_IMAGE.store(tls.image.expose_provenance(), Ordering::Relaxed);
    TLS_FILESZ.store(tls.filesz, Ordering::Relaxed);
    TLS_MEMSZ.store(tls.memsz, Ordering::Relaxed);
    TLS_ALIGN.store(tls.align, Ordering::Relaxed);

    let Some(len) = reservation(tls.memsz, tls.align) else {
        syscall::trap()
    };
    let base = if len <= BUILTIN_LEN {
        BUILTIN.0.get().cast::<u8>()
    } else {
        map(len)
    };
    let (stack_guard, pointer_guard) = guards();
    // SAFETY: the builtin memory and a fresh mapping are zeroed and unused,
    // and the image was recorded above.
    let Some(tp) = (unsafe { build(base, len, stack_guard, pointer_guard) }) else {
        syscall::trap()
    };
    // SAFETY: `build` wrote the control block.
    let state = unsafe { state(tp) };
    state
        .detach_state
        .store(crate::pthread::DT_JOINABLE, Ordering::Relaxed);
    state.tsd.store(crate::key::main_tsd(), Ordering::Relaxed);

    let lock = crate::pthread::THREAD_LIST_LOCK.as_ptr();
    // SAFETY: the lock is a static the kernel may clear at any time after the
    // thread ends. `set_tid_address` returns the caller's id.
    let tid = unsafe { syscall::syscall2(nr::SET_TID_ADDRESS, lock.addr(), 0) };
    // SAFETY: as above.
    unsafe { self::tid(tp) }.store(tid as c_int, Ordering::Relaxed);

    // SAFETY: the new thread pointer is a control block that lives for the
    // rest of the process.
    if unsafe { arch::set_thread_pointer(tp.expose_provenance()) } != 0 {
        syscall::trap();
    }
}

/// The calling thread's control block.
///
/// Before [`init_main`] the thread pointer is zero, and this faults.
pub fn current() -> *mut Thread {
    with_exposed_provenance_mut(arch::thread_pointer())
}

/// The shared state of the thread whose control block is at `t`.
///
/// # Safety
///
/// `t` must be a control block that stays mapped for `'a`.
pub unsafe fn state<'a>(t: *mut Thread) -> &'a State {
    // SAFETY: the caller vouches for the control block. `State` is all
    // atomics, so a shared reference to it permits every write other threads
    // make.
    unsafe { &(*t).state }
}

/// The kernel's id for the thread whose control block is at `t`.
///
/// # Safety
///
/// As [`state`].
pub unsafe fn tid<'a>(t: *mut Thread) -> &'a AtomicI32 {
    // SAFETY: the caller vouches for the control block, and the field is
    // atomic.
    unsafe { &(*t).tid }
}

/// The calling thread's shared state.
///
/// Before [`init_main`] this faults.
pub fn me() -> &'static State {
    // SAFETY: after `init_main` the thread pointer is the calling thread's
    // control block, which lives as long as the thread, and so as long as
    // anything running on it.
    unsafe { state(current()) }
}

/// The calling thread's id.
///
/// Before [`init_main`] this faults.
pub fn my_tid() -> c_int {
    // SAFETY: as in `me`.
    unsafe { tid(current()) }.load(Ordering::Relaxed)
}

/// Records the calling thread's id in its control block again.
///
/// The child of `fork` is a new thread whose control block is a copy of its
/// parent's, and holds the parent's id until this runs.
pub fn refresh_tid() {
    // SAFETY: `gettid` takes no arguments.
    let tid = unsafe { syscall::syscall0(nr::GETTID) } as c_int;
    // SAFETY: after `init_main` the thread pointer is this thread's control
    // block.
    unsafe { self::tid(current()) }.store(tid, Ordering::Relaxed);
}

/// The calling thread's `errno`.
pub fn errno_location() -> *mut c_int {
    current()
        .wrapping_byte_add(offset_of!(Thread, errno))
        .cast::<c_int>()
}

/// Where the calling thread keeps its `uselocale` locale.
pub fn locale_location() -> *mut *mut Locale {
    current()
        .wrapping_byte_add(offset_of!(Thread, locale))
        .cast::<*mut Locale>()
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

    #[test]
    fn a_control_block_leaves_the_main_threads_builtin_memory_room_for_tls() {
        assert!(size_of::<Thread>() <= 272);
    }
}
