//! `obstack.h`: GNU's stacks of objects, grown in chunks the caller's own
//! allocator gives.
//!
//! Most of the interface is macros in the header, which a program built
//! against glibc has inlined: they grow the current object in place while it
//! fits, and call [`_obstack_newchunk`] when it does not. So the structure is
//! glibc's, field for field, and so are the integer widths of its first
//! interface -- `int` lengths and a `long` chunk size -- which glibc keeps for
//! the programs built against it. GMP, which GnuTLS brings into Chrome's
//! window build, calls `_obstack_newchunk` and `obstack_vprintf`.
//!
//! What the functions do is described by the header's contract, not taken
//! from glibc's implementation: a chunk starts with its limit and the chunk
//! before it, objects are aligned to `alignment_mask + 1` in the address
//! space, and freeing an object frees every chunk allocated after it.

use core::ffi::{c_char, c_int, c_long, c_uint, c_void};
use core::mem::size_of;
use core::ptr::null_mut;

use crate::stdio::printf::vasprintf;
use crate::string::memcpy;
use crate::va::{self, VaListArg};

/// The start of each chunk; its objects follow.
#[repr(C)]
#[derive(Debug)]
pub struct Chunk {
    /// One past the chunk's last byte.
    limit: *mut c_char,
    /// The chunk allocated before this one, or null.
    prev: *mut Chunk,
}

/// `struct obstack`, as glibc's `obstack.h` lays it out.
#[repr(C)]
#[derive(Debug)]
pub struct Obstack {
    /// The size a chunk is allocated at, unless an object needs more.
    chunk_size: c_long,
    /// The newest chunk.
    chunk: *mut Chunk,
    /// The start of the object being built.
    object_base: *mut c_char,
    /// Where the object's next byte goes.
    next_free: *mut c_char,
    /// One past the newest chunk's last byte.
    chunk_limit: *mut c_char,
    /// Scratch for the header's macros.
    temp: usize,
    /// One less than the alignment of each object.
    alignment_mask: c_int,
    /// `void *(*)(long)`, or with `use_extra_arg` `void *(*)(void *, long)`.
    chunkfun: *mut c_void,
    /// `void (*)(void *)`, or with `use_extra_arg` `void (*)(void *, void *)`.
    freefun: *mut c_void,
    /// The first argument of both with `use_extra_arg`.
    extra_arg: *mut c_void,
    /// The bit-fields `use_extra_arg`, `maybe_empty_object` and
    /// `alloc_failed`, from the lowest bit up.
    flags: c_uint,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Obstack>() == 88);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Obstack>() == 44);
const _: () = assert!(core::mem::offset_of!(Obstack, alignment_mask) == 6 * size_of::<usize>());

/// `use_extra_arg`.
const USE_EXTRA_ARG: c_uint = 1;
/// `maybe_empty_object`: the newest chunk may hold an empty object that a
/// pointer still refers to, so it must not be freed when an object moves
/// out of it.
const MAYBE_EMPTY_OBJECT: c_uint = 2;

/// The alignment when `_obstack_begin` is given none: `max_align_t`'s.
const DEFAULT_ALIGNMENT: usize = 2 * size_of::<usize>();

/// The chunk size when `_obstack_begin` is given none: a page, less what an
/// allocator keeps beside a block.
const DEFAULT_SIZE: usize = 4096 - 32;

// `obstack_alloc_failed_handler`, null until a program sets it, and
// `obstack_exit_failure`, 1. In assembly, as `gnu.rs`'s variables are, so
// that a program's copy of either is the variable.
#[cfg(all(not(test), target_pointer_width = "64"))]
core::arch::global_asm!(
    ".pushsection .data.ferrousli_obstack,\"aw\"",
    ".p2align 3",
    ".globl obstack_alloc_failed_handler",
    ".type obstack_alloc_failed_handler, %object",
    ".size obstack_alloc_failed_handler, 8",
    "obstack_alloc_failed_handler:",
    ".zero 8",
    ".globl obstack_exit_failure",
    ".type obstack_exit_failure, %object",
    ".size obstack_exit_failure, 4",
    "obstack_exit_failure:",
    ".long 1",
    ".popsection",
);

// The same with a 4-byte pointer.
#[cfg(all(not(test), target_pointer_width = "32"))]
core::arch::global_asm!(
    ".pushsection .data.ferrousli_obstack,\"aw\"",
    ".p2align 2",
    ".globl obstack_alloc_failed_handler",
    ".type obstack_alloc_failed_handler, %object",
    ".size obstack_alloc_failed_handler, 4",
    "obstack_alloc_failed_handler:",
    ".zero 4",
    ".globl obstack_exit_failure",
    ".type obstack_exit_failure, %object",
    ".size obstack_exit_failure, 4",
    "obstack_exit_failure:",
    ".long 1",
    ".popsection",
);

#[cfg(not(test))]
unsafe extern "C" {
    /// `obstack_alloc_failed_handler`, defined above.
    #[link_name = "obstack_alloc_failed_handler"]
    static FAILED_HANDLER: core::sync::atomic::AtomicPtr<c_void>;
    /// `obstack_exit_failure`, defined above.
    #[link_name = "obstack_exit_failure"]
    static EXIT_FAILURE: core::sync::atomic::AtomicI32;
}

/// The variables, in unit tests, where the test binary's own C library
/// defines the C names.
#[cfg(test)]
static FAILED_HANDLER: core::sync::atomic::AtomicPtr<c_void> =
    core::sync::atomic::AtomicPtr::new(null_mut());
/// See [`FAILED_HANDLER`].
#[cfg(test)]
static EXIT_FAILURE: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(1);

/// A chunk could not be allocated: the program's handler, which must not
/// return, or else `memory exhausted` and `exit(obstack_exit_failure)`.
fn failed() -> ! {
    use core::sync::atomic::Ordering;
    #[allow(unused_unsafe, reason = "the variables are Rust statics in unit tests")]
    // SAFETY: the variable lives as long as the process, and is read
    // atomically.
    let handler = unsafe { &FAILED_HANDLER }.load(Ordering::Relaxed);
    if !handler.is_null() {
        // SAFETY: a program sets the variable to a `void (*)(void)`.
        let handler: unsafe extern "C" fn() = unsafe { core::mem::transmute(handler) };
        // SAFETY: the program's own handler, called as it expects.
        unsafe { handler() };
        crate::signal::abort();
    }
    let stderr = crate::stdio::file::stderr.load(Ordering::Relaxed);
    // SAFETY: the message is a NUL-terminated string, and standard error a
    // static stream.
    let _ = unsafe { crate::stdio::io::fputs(c"memory exhausted\n".as_ptr(), stderr) };
    #[allow(unused_unsafe, reason = "the variables are Rust statics in unit tests")]
    // SAFETY: as for the handler.
    let status = unsafe { &EXIT_FAILURE }.load(Ordering::Relaxed);
    crate::exit::exit(status)
}

/// `address` rounded up to a multiple of `mask + 1`.
fn align(address: *mut c_char, mask: c_int) -> *mut c_char {
    let mask = mask as usize;
    let offset = (address.addr().wrapping_add(mask) & !mask).wrapping_sub(address.addr());
    address.wrapping_add(offset)
}

/// Where a chunk's objects may start, before alignment.
fn contents(chunk: *mut Chunk) -> *mut c_char {
    chunk.cast::<c_char>().wrapping_add(size_of::<Chunk>())
}

/// A chunk of `size` bytes from the obstack's allocator, or [`failed`].
///
/// # Safety
///
/// `h` must be an obstack whose `chunkfun` and `extra_arg` are set.
unsafe fn allocate(h: &Obstack, size: usize) -> *mut Chunk {
    let Ok(size) = c_long::try_from(size) else {
        failed();
    };
    let chunk = if h.flags & USE_EXTRA_ARG != 0 {
        // SAFETY: with `use_extra_arg`, the function takes the extra argument.
        let alloc: unsafe extern "C" fn(*mut c_void, c_long) -> *mut c_void =
            unsafe { core::mem::transmute(h.chunkfun) };
        // SAFETY: the program's allocator, called as it was declared.
        unsafe { alloc(h.extra_arg, size) }
    } else {
        // SAFETY: without it, the function takes the size alone.
        let alloc: unsafe extern "C" fn(c_long) -> *mut c_void =
            unsafe { core::mem::transmute(h.chunkfun) };
        // SAFETY: as above.
        unsafe { alloc(size) }
    };
    if chunk.is_null() {
        failed();
    }
    chunk.cast()
}

/// Gives `chunk` back to the obstack's allocator.
///
/// # Safety
///
/// `h` must be an obstack whose `freefun` and `extra_arg` are set, and
/// `chunk` one of its chunks, not used again.
unsafe fn release(h: &Obstack, chunk: *mut Chunk) {
    if h.flags & USE_EXTRA_ARG != 0 {
        // SAFETY: with `use_extra_arg`, the function takes the extra argument.
        let free: unsafe extern "C" fn(*mut c_void, *mut c_void) =
            unsafe { core::mem::transmute(h.freefun) };
        // SAFETY: the program's allocator, given what it allocated.
        unsafe { free(h.extra_arg, chunk.cast()) };
    } else {
        // SAFETY: without it, the function takes the chunk alone.
        let free: unsafe extern "C" fn(*mut c_void) = unsafe { core::mem::transmute(h.freefun) };
        // SAFETY: as above.
        unsafe { free(chunk.cast()) };
    }
}

/// `_obstack_begin` and `_obstack_begin_1`, once the functions are stored.
///
/// # Safety
///
/// `h` must be valid for writes of an obstack, and the functions stored in it
/// an allocator and its release.
unsafe fn begin(h: &mut Obstack, size: c_int, alignment: c_int) -> c_int {
    let alignment = usize::try_from(alignment)
        .ok()
        .filter(|&a| a != 0)
        .unwrap_or(DEFAULT_ALIGNMENT);
    let size = usize::try_from(size)
        .ok()
        .filter(|&s| s != 0)
        .unwrap_or(DEFAULT_SIZE);
    h.chunk_size = size as c_long;
    h.alignment_mask = (alignment - 1) as c_int;
    // SAFETY: the caller stored the functions.
    let chunk = unsafe { allocate(h, size) };
    let limit = chunk.cast::<c_char>().wrapping_add(size);
    // SAFETY: the allocator gave `size` bytes, more than a chunk's header.
    unsafe {
        chunk.write(Chunk {
            limit,
            prev: null_mut(),
        });
    }
    h.chunk = chunk;
    h.object_base = align(contents(chunk), h.alignment_mask);
    h.next_free = h.object_base;
    h.chunk_limit = limit;
    h.flags &= USE_EXTRA_ARG;
    1
}

/// Starts the obstack `*h`, whose chunks are `size` bytes (a default for 0),
/// its objects aligned to `alignment` (a default for 0), allocated by
/// `chunkfun(size)` and given back by `freefun(chunk)`. 1; an allocation that
/// fails calls `obstack_alloc_failed_handler`.
///
/// # Safety
///
/// `h` must be valid for writes of a `struct obstack`, and the functions an
/// allocator and its release.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _obstack_begin(
    h: *mut Obstack,
    size: c_int,
    alignment: c_int,
    chunkfun: *mut c_void,
    freefun: *mut c_void,
) -> c_int {
    // SAFETY: the caller passes a writable obstack.
    let h = unsafe { &mut *h };
    h.chunkfun = chunkfun;
    h.freefun = freefun;
    h.extra_arg = null_mut();
    h.flags = 0;
    // SAFETY: the functions are stored.
    unsafe { begin(h, size, alignment) }
}

/// [`_obstack_begin`], with `chunkfun(arg, size)` and `freefun(arg, chunk)`.
///
/// # Safety
///
/// As [`_obstack_begin`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _obstack_begin_1(
    h: *mut Obstack,
    size: c_int,
    alignment: c_int,
    chunkfun: *mut c_void,
    freefun: *mut c_void,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: the caller passes a writable obstack.
    let h = unsafe { &mut *h };
    h.chunkfun = chunkfun;
    h.freefun = freefun;
    h.extra_arg = arg;
    h.flags = USE_EXTRA_ARG;
    // SAFETY: the functions are stored.
    unsafe { begin(h, size, alignment) }
}

/// Moves the object being built to a new chunk with room for `length` more
/// bytes, and frees the old chunk if the object was all it held.
///
/// # Safety
///
/// `h` must be a started obstack.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _obstack_newchunk(h: *mut Obstack, length: c_int) {
    // SAFETY: the caller passes a started obstack.
    let h = unsafe { &mut *h };
    let old = h.chunk;
    let object = h.next_free.addr().wrapping_sub(h.object_base.addr());
    let needed = usize::try_from(length)
        .ok()
        .and_then(|length| object.checked_add(length))
        .and_then(|n| n.checked_add(object >> 3))
        .and_then(|n| n.checked_add(h.alignment_mask as usize + 100));
    let Some(needed) = needed else {
        failed();
    };
    let size = needed.max(h.chunk_size as usize);
    // SAFETY: a started obstack has its functions.
    let chunk = unsafe { allocate(h, size) };
    let limit = chunk.cast::<c_char>().wrapping_add(size);
    // SAFETY: the allocator gave `size` bytes.
    unsafe { chunk.write(Chunk { limit, prev: old }) };
    let base = align(contents(chunk), h.alignment_mask);
    // SAFETY: the object is `object` bytes, and the new chunk has room for
    // them past `base`; the two chunks are different allocations.
    let _ = unsafe { memcpy(base.cast(), h.object_base.cast(), object) };
    if h.flags & MAYBE_EMPTY_OBJECT == 0 && h.object_base == align(contents(old), h.alignment_mask)
    {
        // The object was the old chunk's only one: nothing else points into
        // it, so it goes.
        // SAFETY: `old` is a chunk of this obstack, still live.
        let older = unsafe { (*old).prev };
        // SAFETY: the new chunk was just written.
        unsafe { (*chunk).prev = older };
        // SAFETY: `old` is not used again.
        unsafe { release(h, old) };
    }
    h.chunk = chunk;
    h.chunk_limit = limit;
    h.object_base = base;
    h.next_free = base.wrapping_add(object);
    h.flags &= !MAYBE_EMPTY_OBJECT;
}

/// Frees `obj` and every object allocated after it, or with a null `obj` the
/// whole obstack. A pointer that is in none of its chunks aborts.
///
/// # Safety
///
/// `h` must be a started obstack, and `obj` null or an object in it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn obstack_free(h: *mut Obstack, obj: *mut c_void) {
    // SAFETY: the caller passes a started obstack.
    let h = unsafe { &mut *h };
    let at = obj.addr();
    let mut chunk = h.chunk;
    // SAFETY: each chunk in the list is live until it is released here.
    while !chunk.is_null() && (chunk.addr() >= at || unsafe { (*chunk).limit }.addr() < at) {
        // SAFETY: as above.
        let prev = unsafe { (*chunk).prev };
        // SAFETY: the chunk holds only objects after `obj`.
        unsafe { release(h, chunk) };
        chunk = prev;
        h.flags |= MAYBE_EMPTY_OBJECT;
    }
    if chunk.is_null() {
        if !obj.is_null() {
            crate::signal::abort();
        }
        h.chunk = null_mut();
        h.object_base = null_mut();
        h.next_free = null_mut();
        h.chunk_limit = null_mut();
        return;
    }
    h.object_base = obj.cast();
    h.next_free = obj.cast();
    // SAFETY: the chunk is live.
    h.chunk_limit = unsafe { (*chunk).limit };
    h.chunk = chunk;
}

/// glibc's older name for [`obstack_free`].
///
/// # Safety
///
/// As [`obstack_free`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _obstack_free(h: *mut Obstack, obj: *mut c_void) {
    // SAFETY: the caller's contract is `obstack_free`'s.
    unsafe { obstack_free(h, obj) }
}

/// Whether `obj` is inside one of the obstack's chunks: 1 or 0.
///
/// # Safety
///
/// `h` must be a started obstack.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _obstack_allocated_p(h: *mut Obstack, obj: *mut c_void) -> c_int {
    let at = obj.addr();
    // SAFETY: the caller passes a started obstack.
    let mut chunk = unsafe { (*h).chunk };
    // SAFETY: every chunk in the list is live.
    while !chunk.is_null() && (chunk.addr() >= at || unsafe { (*chunk).limit }.addr() < at) {
        // SAFETY: as above.
        chunk = unsafe { (*chunk).prev };
    }
    c_int::from(!chunk.is_null())
}

/// The bytes the obstack's chunks take, their headers included.
///
/// # Safety
///
/// `h` must be a started obstack.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _obstack_memory_used(h: *mut Obstack) -> c_int {
    let mut total = 0_usize;
    // SAFETY: the caller passes a started obstack.
    let mut chunk = unsafe { (*h).chunk };
    while !chunk.is_null() {
        // SAFETY: every chunk in the list is live.
        let limit = unsafe { (*chunk).limit };
        // SAFETY: as above.
        let prev = unsafe { (*chunk).prev };
        total = total.wrapping_add(limit.addr().wrapping_sub(chunk.addr()));
        chunk = prev;
    }
    total as c_int
}

/// Appends the formatted text, without a NUL, to the object being built,
/// and answers its length, or -1 when it cannot be formatted.
///
/// # Safety
///
/// `h` must be a started obstack, and `ap`'s arguments match `fmt`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn obstack_vprintf(
    h: *mut Obstack,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    let mut text: *mut c_char = null_mut();
    // SAFETY: the caller's contract is `vasprintf`'s.
    let length = unsafe { vasprintf(&raw mut text, fmt, ap) };
    let Ok(bytes) = usize::try_from(length) else {
        return -1;
    };
    // SAFETY: the caller passes a started obstack.
    let obstack = unsafe { &mut *h };
    let room = obstack
        .chunk_limit
        .addr()
        .wrapping_sub(obstack.next_free.addr());
    if room < bytes {
        // SAFETY: a started obstack.
        unsafe { _obstack_newchunk(obstack, length) };
    }
    // SAFETY: there is room for `bytes` at `next_free`, and `text` holds
    // them; the two are different allocations.
    let _ = unsafe { memcpy(obstack.next_free.cast(), text.cast(), bytes) };
    obstack.next_free = obstack.next_free.wrapping_add(bytes);
    // SAFETY: `vasprintf` allocated `text` with the process's `malloc`.
    unsafe { crate::malloc::free(text.cast()) };
    length
}

/// The fortified [`obstack_vprintf`]; the flag asks it to refuse `%n` in a
/// writable format, which this library never writes through anyway.
///
/// # Safety
///
/// As [`obstack_vprintf`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __obstack_vprintf_chk(
    h: *mut Obstack,
    _flag: c_int,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller's contract is `obstack_vprintf`'s.
    unsafe { obstack_vprintf(h, fmt, ap) }
}

va::variadic!(obstack_printf, 2, obstack_vprintf);
va::variadic!(__obstack_printf_chk, 3, __obstack_vprintf_chk);

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn chunk_alloc(size: c_long) -> *mut c_void {
        crate::malloc::malloc(size as usize)
    }

    unsafe extern "C" fn chunk_free(chunk: *mut c_void) {
        // SAFETY: the chunk came from `chunk_alloc`.
        unsafe { crate::malloc::free(chunk) }
    }

    fn started(size: c_int) -> Obstack {
        // SAFETY: an all-zero obstack is what a C program declares.
        let mut h: Obstack = unsafe { core::mem::zeroed() };
        // SAFETY: the functions are an allocator and its release.
        let ok = unsafe {
            _obstack_begin(
                &raw mut h,
                size,
                0,
                chunk_alloc as *mut c_void,
                chunk_free as *mut c_void,
            )
        };
        assert_eq!(ok, 1);
        h
    }

    /// `obstack_grow`, as the header's macro does it.
    fn grow(h: &mut Obstack, bytes: &[u8]) {
        let room = h.chunk_limit.addr() - h.next_free.addr();
        if room < bytes.len() {
            // SAFETY: a started obstack.
            unsafe { _obstack_newchunk(h, bytes.len() as c_int) };
        }
        for &byte in bytes {
            // SAFETY: there is room.
            unsafe { h.next_free.cast::<u8>().write(byte) };
            h.next_free = h.next_free.wrapping_add(1);
        }
    }

    /// `obstack_finish`, as the header's macro does it.
    fn finish(h: &mut Obstack) -> *mut c_char {
        let object = h.object_base;
        h.next_free = align(h.next_free, h.alignment_mask);
        if h.next_free > h.chunk_limit {
            h.next_free = h.chunk_limit;
        }
        h.object_base = h.next_free;
        object
    }

    #[test]
    fn an_object_that_outgrows_its_chunk_moves_whole_and_stays_aligned() {
        let mut h = started(64);
        grow(&mut h, b"first");
        let first = finish(&mut h);
        let big = [b'x'; 300];
        grow(&mut h, b"ab");
        grow(&mut h, &big);
        let second = finish(&mut h);
        assert_eq!(second.addr() % DEFAULT_ALIGNMENT, 0);
        // SAFETY: both objects are live in the obstack.
        let first_bytes = unsafe { core::slice::from_raw_parts(first.cast::<u8>(), 5) };
        assert_eq!(first_bytes, b"first");
        // SAFETY: as above.
        let second_bytes = unsafe { core::slice::from_raw_parts(second.cast::<u8>(), 302) };
        assert_eq!(second_bytes.get(..2), Some(&b"ab"[..]));
        assert_eq!(second_bytes.get(301), Some(&b'x'));
        // SAFETY: a started obstack.
        let first_in = unsafe { _obstack_allocated_p(&raw mut h, first.cast()) };
        // SAFETY: as above.
        let second_in = unsafe { _obstack_allocated_p(&raw mut h, second.cast()) };
        // SAFETY: as above.
        let used = unsafe { _obstack_memory_used(&raw mut h) };
        assert_eq!((first_in, second_in), (1, 1));
        assert!(used >= 64 + 302);
        // Freeing the first frees the second's chunk too.
        // SAFETY: `first` is an object in it.
        unsafe { obstack_free(&raw mut h, first.cast()) };
        assert_eq!(h.next_free, first);
        // SAFETY: a started obstack.
        let second_in = unsafe { _obstack_allocated_p(&raw mut h, second.cast()) };
        assert_eq!(second_in, 0);
        // SAFETY: the whole obstack goes.
        unsafe { obstack_free(&raw mut h, null_mut()) };
        assert!(h.chunk.is_null());
    }

    #[test]
    fn a_growing_object_alone_in_its_chunk_leaves_no_chunk_behind() {
        let mut h = started(64);
        grow(&mut h, &[1; 40]);
        let before = h.chunk;
        grow(&mut h, &[2; 100]);
        // SAFETY: the new chunk is live.
        let older = unsafe { (*h.chunk).prev };
        assert!(older.is_null(), "the old chunk {before:?} was kept");
        // SAFETY: the whole obstack goes.
        unsafe { obstack_free(&raw mut h, null_mut()) };
    }
}
