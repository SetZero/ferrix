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
//! [`MODULE_OFFSETS`], with no dynamic thread vector and no allocation.
//!
//! # A module `dlopen` brings
//!
//! The start-up layout leaves [`crate::scope::TLS_SURPLUS`] bytes of every
//! thread's static TLS unused, as glibc's does, and a library `dlopen` loads
//! gets its block there: an offset from the thread pointer like any other,
//! so `__tls_get_addr` answers it from the same table. What differs is that
//! threads already running have that block zeroed, not holding its image.
//! So each such module has a generation, its place in the order they were
//! opened ([`MODULE_GENERATIONS`]), and each thread keeps in its control
//! block's `dtv` word how many it has initialised -- zero in a new thread,
//! which is what every C library here leaves there. `__tls_get_addr` for a
//! module newer than that first copies the images of every module the
//! thread has not seen into its blocks ([`__ferrousli_tls_catch_up`]).
//! A thread reaches such a module's variables only through that call, so
//! nothing it has written is overwritten.
//!
//! Initial-exec and descriptor accesses bypass `__tls_get_addr`. So, as
//! glibc does, `dlopen` also copies a new module's image into every thread
//! at once ([`make_ready`]), through a walk over its threads the C library
//! registers ([`set_thread_walk`]), and the C library catches up each thread
//! it starts before it runs. libglvnd's `libGL.so.1` reads its dispatch
//! table by initial-exec. Without a registered walk, a module reached that
//! way whose image is not all zeros -- zeros being what the surplus holds in
//! every thread -- is still refused ([`needs_catch_up`]).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

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

/// Each module's generation, by module number: zero for a start-up module,
/// and for a module `dlopen` brought its place in the order they were
/// opened, from 1. Read by `__tls_get_addr` from assembly.
static MODULE_GENERATIONS: Offsets = Offsets(UnsafeCell::new([0; MODULES]));

/// The most modules with TLS `dlopen` may bring.
const MAX_DYNAMIC: usize = 32;

/// A module `dlopen` brought: where its block is, and what fills it.
#[derive(Clone, Copy)]
struct Dynamic {
    offset: isize,
    image: usize,
    filesz: usize,
    memsz: usize,
}

/// The modules `dlopen` brought, in generation order, and how many.
struct DynamicModules(UnsafeCell<[Dynamic; MAX_DYNAMIC]>);

// SAFETY: an entry is written once, under `dlopen`'s lock, before the count
// that covers it is published with release ordering; readers read only
// entries below a count they loaded with acquire ordering.
unsafe impl Sync for DynamicModules {}

/// See [`DynamicModules`].
static DYNAMIC: DynamicModules = DynamicModules(UnsafeCell::new(
    [Dynamic {
        offset: 0,
        image: 0,
        filesz: 0,
        memsz: 0,
    }; MAX_DYNAMIC],
));

/// How many entries of [`DYNAMIC`] are published: the newest generation a
/// thread may catch up on, whose object is relocated.
static DYNAMIC_COUNT: AtomicUsize = AtomicUsize::new(0);

/// How many entries of [`DYNAMIC`] are written: [`DYNAMIC_COUNT`], and the
/// modules the `dlopen` under way placed, which [`make_ready`] publishes once
/// they are relocated -- a module's image may itself be relocated, and a
/// thread must not copy it before. Written only under `dlopen`'s lock.
static PLACED: AtomicUsize = AtomicUsize::new(0);

/// What the C library calls for each of its threads' thread pointers.
pub(crate) type Visit = unsafe extern "C" fn(tp: *mut u8);

/// The C library's walk over every thread it started, the calling one
/// included, with its thread list locked.
pub(crate) type Walk = unsafe extern "C" fn(visit: Visit);

/// The C library's [`Walk`], once it has registered one; null before.
static WALK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Record the C library's walk over its threads, so that `dlopen` can give
/// every thread a new module's image, as glibc does, and initial-exec and
/// descriptor accesses to it need no catching up. `None` forgets it.
pub(crate) unsafe extern "C" fn set_thread_walk(walk: Option<Walk>) {
    let walk = walk.map_or(core::ptr::null_mut(), |walk| walk as *mut ());
    WALK.store(walk, Ordering::Release);
}

/// The registered walk, if there is one.
fn walk() -> Option<Walk> {
    let walk = WALK.load(Ordering::Acquire);
    // SAFETY: a non-null value is a `Walk` that `set_thread_walk` stored, and
    // a function pointer and a data pointer are one size here.
    (!walk.is_null()).then(|| unsafe { core::mem::transmute::<*mut (), Walk>(walk) })
}

/// Publish object `index`, which `dlopen` just placed in the surplus, as the
/// next generation: its offset and generation for `__tls_get_addr`, and its
/// image for the threads that catch up once [`make_ready`] runs.
///
/// # Errors
///
/// [`Error::TooManyObjects`] past [`MAX_DYNAMIC`] modules with TLS.
///
/// # Safety
///
/// Called under `dlopen`'s lock, before anything can reach the module.
pub(crate) unsafe fn publish_dynamic(scope: &Scope, index: usize) -> Result<(), Error> {
    if index + 1 >= MODULES {
        return Err(Error::TooManyObjects);
    }
    let generation = MODULE_GENERATIONS
        .0
        .get()
        .cast::<isize>()
        .wrapping_add(index + 1);
    let Some(tls) = scope.get(index).and_then(|object| object.tls) else {
        // SAFETY: `index + 1` is inside the table, and the lock makes this the
        // only writer. A failed `dlopen` may have left the index a module's.
        unsafe { generation.write(0) };
        return Ok(());
    };
    let count = PLACED.load(Ordering::Relaxed);
    if count >= MAX_DYNAMIC {
        return Err(Error::TooManyObjects);
    }
    let entry = DYNAMIC.0.get().cast::<Dynamic>().wrapping_add(count);
    // SAFETY: the lock makes this the only writer, and entry `count` is
    // published below, after it is written.
    unsafe {
        entry.write(Dynamic {
            offset: tls.offset,
            image: tls.image,
            filesz: tls.filesz,
            memsz: tls.memsz,
        });
    }
    let offset = MODULE_OFFSETS
        .0
        .get()
        .cast::<isize>()
        .wrapping_add(index + 1);
    // SAFETY: `index + 1` is inside the table, and the lock makes this the
    // only writer; nothing reads the module's entry before it is relocated.
    unsafe { offset.write(tls.offset) };
    // SAFETY: as above.
    unsafe { generation.write(count as isize + 1) };
    PLACED.store(count + 1, Ordering::Relaxed);
    Ok(())
}

/// Publish the generations this `dlopen` placed, its objects now relocated,
/// and copy their images into every thread the C library has started.
///
/// # Safety
///
/// Called under `dlopen`'s lock, after the new objects are relocated and
/// before their initialisers run.
pub(crate) unsafe fn make_ready() {
    DYNAMIC_COUNT.store(PLACED.load(Ordering::Relaxed), Ordering::Release);
    if let Some(walk) = walk() {
        // SAFETY: the C library visits each live thread's pointer, with its
        // list locked, so none is freed under the copy.
        unsafe { walk(catch_up_thread) };
    }
}

/// Forget the generations a failed `dlopen` placed.
///
/// # Safety
///
/// Called under `dlopen`'s lock.
pub(crate) unsafe fn abandon() {
    PLACED.store(DYNAMIC_COUNT.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Whether module number `module` is one `dlopen` brought whose image is not
/// all zeros, with no C library walk to give it to every thread: one a thread
/// must catch up on before it reads it, which only `__tls_get_addr` does.
pub(crate) fn needs_catch_up(module: usize) -> bool {
    if module >= MODULES || walk().is_some() {
        return false;
    }
    // SAFETY: `module` is inside the table, which is written only under
    // `dlopen`'s lock, which the relocation asking this runs under.
    let generation = unsafe {
        MODULE_GENERATIONS
            .0
            .get()
            .cast::<isize>()
            .wrapping_add(module)
            .read()
    };
    if generation <= 0 {
        return false;
    }
    // SAFETY: entry `generation - 1` was written before its generation was.
    let entry = unsafe {
        DYNAMIC
            .0
            .get()
            .cast::<Dynamic>()
            .wrapping_add(generation as usize - 1)
            .read()
    };
    (0..entry.filesz).any(|at| {
        // SAFETY: the image is `filesz` mapped bytes.
        let byte = unsafe { (entry.image as *const u8).wrapping_add(at).read() };
        byte != 0
    })
}

/// The count of initialised generations of the thread whose pointer is `tp`:
/// its control block's `dtv` word, which no C library here uses otherwise.
fn generation_word(tp: *mut u8) -> *mut usize {
    #[cfg(target_arch = "x86_64")]
    return tp.wrapping_add(size_of::<usize>()).cast();
    #[cfg(not(target_arch = "x86_64"))]
    return tp.cast();
}

/// Bring the calling thread's blocks of every module `dlopen` brought since
/// it last looked up to date. Called by `__tls_get_addr` when a module is
/// newer than the thread.
#[unsafe(no_mangle)]
unsafe extern "C" fn __ferrousli_tls_catch_up() {
    // SAFETY: the calling thread's own pointer.
    unsafe { catch_up_thread(thread_pointer() as *mut u8) };
}

/// Bring the blocks of the thread whose pointer is `tp` up to date with every
/// module `dlopen` brought since it last did: each image copied, the rest of
/// its block zeroed, and its count of generations advanced.
///
/// The C library calls it for a thread it is starting, with its thread list
/// locked and before the thread runs, and [`make_ready`] for every thread; a
/// thread calls it for itself from `__tls_get_addr`. None of them writes a
/// block the thread has used: no thread reaches a module before `dlopen`
/// returns it, and [`make_ready`] has copied it everywhere by then.
///
/// # Safety
///
/// `tp` must be a live thread's pointer, with the loader's static TLS beside
/// it, whose control block's `dtv` word no one else is writing.
pub(crate) unsafe extern "C" fn catch_up_thread(tp: *mut u8) {
    let word = generation_word(tp);
    // SAFETY: the word is in the thread's control block.
    let seen = unsafe { word.read() };
    let count = DYNAMIC_COUNT.load(Ordering::Acquire);
    for generation in seen..count {
        // SAFETY: entries below the count were written before it.
        let entry = unsafe {
            DYNAMIC
                .0
                .get()
                .cast::<Dynamic>()
                .wrapping_add(generation)
                .read()
        };
        let block = tp.wrapping_offset(entry.offset);
        // SAFETY: the block is this thread's, `memsz` bytes in its static TLS
        // surplus, and the image `filesz` mapped bytes.
        unsafe { core::ptr::copy_nonoverlapping(entry.image as *const u8, block, entry.filesz) };
        // SAFETY: the rest of the same block.
        unsafe {
            core::ptr::write_bytes(
                block.wrapping_add(entry.filesz),
                0,
                entry.memsz - entry.filesz,
            );
        }
    }
    // SAFETY: as above.
    unsafe { word.write(count) };
}

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
    "    lea rcx, [rip + {generations}]",
    "    mov rcx, qword ptr [rcx + 8*rax]",
    "    test rcx, rcx",
    "    jnz 3f",
    "1:",
    "    lea rcx, [rip + {offsets}]",
    "    mov rax, qword ptr [rcx + 8*rax]",
    "    add rax, qword ptr [rdi + 8]",
    "    add rax, qword ptr fs:[0]",
    "    ret",
    // A module `dlopen` brought, newer than this thread has seen: catch
    // up, on a stack aligned for the call, keeping the argument and index.
    "3:",
    "    cmp rcx, qword ptr fs:[8]",
    "    jbe 1b",
    "    push rdi",
    "    push rax",
    "    push rbp",
    "    mov rbp, rsp",
    "    and rsp, -16",
    "    call {catch_up}",
    "    mov rsp, rbp",
    "    pop rbp",
    "    pop rax",
    "    pop rdi",
    "    jmp 1b",
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
    generations = sym MODULE_GENERATIONS,
    catch_up = sym __ferrousli_tls_catch_up,
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
    "    adrp x2, {generations}",
    "    add x2, x2, :lo12:{generations}",
    "    ldr x2, [x2, x1, lsl #3]",
    "    cbnz x2, 3f",
    "1:",
    "    adrp x2, {offsets}",
    "    add x2, x2, :lo12:{offsets}",
    "    ldr x1, [x2, x1, lsl #3]",
    "    ldr x2, [x0, #8]",
    "    add x1, x1, x2",
    "    mrs x2, tpidr_el0",
    "    add x0, x1, x2",
    "    ret",
    // A module `dlopen` brought, newer than this thread has seen.
    "3:",
    "    mrs x3, tpidr_el0",
    "    ldr x3, [x3]",
    "    cmp x2, x3",
    "    b.ls 1b",
    "    stp x29, x30, [sp, #-32]!",
    "    mov x29, sp",
    "    stp x0, x1, [sp, #16]",
    "    bl {catch_up}",
    "    ldp x0, x1, [sp, #16]",
    "    ldp x29, x30, [sp], #32",
    "    b 1b",
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
    generations = sym MODULE_GENERATIONS,
    catch_up = sym __ferrousli_tls_catch_up,
);

/// The last module number ARMv7-A's `__tls_get_addr` answers for. It
/// compares with this rather than with [`MODULES`], branching when above it,
/// because an ARM immediate is eight bits rotated right by an even amount:
/// 256 is one, and 257 -- `MODULES` since the loader holds 256 objects -- is
/// not, and does not assemble.
#[cfg(target_arch = "arm")]
const LAST_MODULE: usize = MODULES - 1;

/// Whether `value` is an ARM-state data-processing immediate.
#[cfg(target_arch = "arm")]
const fn arm_immediate(value: usize) -> bool {
    let value = value as u32;
    let mut rotation = 0;
    while rotation < 32 {
        if value.rotate_left(rotation) <= 0xff {
            return true;
        }
        rotation += 2;
    }
    false
}

#[cfg(target_arch = "arm")]
const _: () = assert!(arm_immediate(LAST_MODULE));

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
    "    cmp r1, #{last}",
    "    bhi 2f",
    "    ldr r2, 5f",
    "6:",
    "    add r2, pc, r2",
    "    ldr r2, [r2, r1, lsl #2]",
    "    cmp r2, #0",
    "    bne 7f",
    "1:",
    "    ldr r2, 3f",
    "4:",
    "    add r2, pc, r2",
    "    ldr r1, [r2, r1, lsl #2]",
    "    ldr r2, [r0, #4]",
    "    add r1, r1, r2",
    "    mrc p15, 0, r2, c13, c0, 3",
    "    add r0, r1, r2",
    "    bx lr",
    // A module `dlopen` brought, newer than this thread has seen; four
    // words keep the stack's eight-byte alignment.
    "7:",
    "    mrc p15, 0, r3, c13, c0, 3",
    "    ldr r3, [r3]",
    "    cmp r2, r3",
    "    bls 1b",
    "    push {{r0, r1, r2, lr}}",
    "    bl {catch_up}",
    "    pop {{r0, r1, r2, lr}}",
    "    b 1b",
    "2:",
    "    udf #0",
    "3:",
    "    .word {offsets} - (4b + 8)",
    "5:",
    "    .word {generations} - (6b + 8)",
    ".size __tls_get_addr, . - __tls_get_addr",
    ".popsection",
    last = const LAST_MODULE,
    offsets = sym MODULE_OFFSETS,
    generations = sym MODULE_GENERATIONS,
    catch_up = sym __ferrousli_tls_catch_up,
);

/// `PROT_READ | PROT_WRITE`.
const PROT_READ_WRITE: usize = 0x1 | 0x2;
/// `MAP_PRIVATE | MAP_ANONYMOUS`.
const MAP_PRIVATE_ANONYMOUS: usize = 0x2 | 0x20;
/// glibc's alignment for the initial control block.
pub(crate) const TP_ALIGN: usize = 64;

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
