//! AArch64's memory and string routines: sixteen bytes at a time, in Advanced
//! SIMD registers.
//!
//! Advanced SIMD is on every AArch64 core Linux runs on -- the procedure call
//! standard passes floating point in its registers -- so nothing here asks
//! `AT_HWCAP` first. The routines are shaped for the cores of the Pixel 7's
//! Tensor G2 (Cortex-X1, A78 and A55), where a sixteen-byte load or store
//! costs what a byte's does, and they are no slower on QEMU's `virt` machine,
//! where Ferrix's gates run them. The other architectures keep the word loops
//! in the parent module.
//!
//! # Loads and stores are assembly
//!
//! `core`'s `vld1q_u8` and `vst1q_u8` are an unaligned read and write, which
//! an unoptimised build lowers to a call to `memcpy`: infinite recursion
//! inside `memcpy` itself, and the library must work unoptimised (see the
//! parent module). So a vector moves through `ldr` and `str` in `asm!`, and
//! intrinsics do only the arithmetic, which touches no memory.
//!
//! The intrinsics are safe to call only from a function that enables `neon`
//! itself, even though the target enables it for the whole library, so the
//! routines that use them say `#[target_feature(enable = "neon")]`.
//!
//! # The bulk loops
//!
//! A copy or a fill of 64 bytes or more moves them in one `asm!` loop of
//! register pairs, `ldp` and `stp` of two `q` registers with the address
//! stepped by the instruction itself: five instructions for 64 bytes of copy,
//! four for 64 bytes of fill. Measured with QEMU's instruction-count plugin
//! on a Cortex-A55, that is under half of what LLVM makes of the parent
//! module's byte loops, which it vectorises by itself in a release build,
//! and a third of what sixteen-byte `ldr` and `str` in separate `asm!`
//! blocks cost, since the compiler cannot pair or step those.
//!
//! # Overlapping ends
//!
//! A copy, a fill or a comparison of sixteen bytes or more does its last
//! block at the very end of the range, overlapping the block before it,
//! rather than finishing a byte at a time. A copy reads that last block
//! before it writes anything, which is also what makes a forward copy safe
//! for `memmove` when the destination is below the source.
//!
//! # Reading past the end
//!
//! The searches read whole aligned blocks, as the parent module's word loops
//! read aligned words: an aligned block never crosses a page, so a block
//! holding one byte the caller vouched for is readable whole. Bytes outside
//! the caller's range are masked off and never reported.

use core::arch::aarch64::{
    uint8x8_t, uint8x16_t, vceqq_u8, vdup_n_u8, vdupq_n_u8, vget_lane_u64, vmaxvq_u8, vmvnq_u8,
    vorrq_u8, vreinterpret_u64_u8, vreinterpretq_u16_u8, vshrn_n_u16,
};
use core::arch::asm;
use core::ffi::c_int;

/// The bytes in a vector register, and the step of every loop here.
const BLOCK: usize = 16;

/// Four blocks: the step of the copy and fill loops, which load or store
/// four registers in a row.
const RUN: usize = 4 * BLOCK;

/// The sixteen bytes at `p`.
///
/// # Safety
///
/// The sixteen bytes at `p` must be readable. Normal memory takes a `q`
/// register's load at any alignment.
#[inline(always)]
unsafe fn load(p: *const u8) -> uint8x16_t {
    let v: uint8x16_t;
    // SAFETY: the caller vouches for the sixteen bytes; the load writes only
    // `v`.
    unsafe {
        asm!(
            "ldr {v:q}, [{p}]",
            p = in(reg) p,
            v = out(vreg) v,
            options(nostack, readonly, preserves_flags),
        );
    }
    v
}

/// Writes `v` over the sixteen bytes at `p`.
///
/// # Safety
///
/// The sixteen bytes at `p` must be writable.
#[inline(always)]
unsafe fn store(p: *mut u8, v: uint8x16_t) {
    // SAFETY: the caller vouches for the sixteen bytes.
    unsafe {
        asm!(
            "str {v:q}, [{p}]",
            p = in(reg) p,
            v = in(vreg) v,
            options(nostack, preserves_flags),
        );
    }
}

/// The eight bytes at `p`.
///
/// # Safety
///
/// The eight bytes at `p` must be readable.
#[inline(always)]
unsafe fn load8(p: *const u8) -> uint8x8_t {
    let v: uint8x8_t;
    // SAFETY: the caller vouches for the eight bytes.
    unsafe {
        asm!(
            "ldr {v:d}, [{p}]",
            p = in(reg) p,
            v = out(vreg) v,
            options(nostack, readonly, preserves_flags),
        );
    }
    v
}

/// Writes `v` over the eight bytes at `p`.
///
/// # Safety
///
/// The eight bytes at `p` must be writable.
#[inline(always)]
unsafe fn store8(p: *mut u8, v: uint8x8_t) {
    // SAFETY: the caller vouches for the eight bytes.
    unsafe {
        asm!(
            "str {v:d}, [{p}]",
            p = in(reg) p,
            v = in(vreg) v,
            options(nostack, preserves_flags),
        );
    }
}

/// Copies `runs` runs of 64 bytes from `from` to `to`, lowest first. Each
/// run's 64 bytes are all read before any is written.
///
/// # Safety
///
/// `runs` must be at least one, both ranges valid for `runs * 64` bytes, and
/// if they overlap, `to` must not be above `from`.
#[inline(always)]
unsafe fn copy_runs_forward(to: *mut u8, from: *const u8, runs: usize) {
    // SAFETY: the caller vouches for both ranges and for `runs`; the loop
    // touches nothing else, and clobbers only what it names.
    unsafe {
        asm!(
            "2:",
            "ldp {a:q}, {b:q}, [{from}], #32",
            "ldp {c:q}, {d:q}, [{from}], #32",
            "stp {a:q}, {b:q}, [{to}], #32",
            "stp {c:q}, {d:q}, [{to}], #32",
            "subs {runs}, {runs}, #1",
            "b.ne 2b",
            from = inout(reg) from => _,
            to = inout(reg) to => _,
            runs = inout(reg) runs => _,
            a = out(vreg) _,
            b = out(vreg) _,
            c = out(vreg) _,
            d = out(vreg) _,
            options(nostack),
        );
    }
}

/// Copies `runs` runs of 64 bytes ending at `from_end` to the bytes ending at
/// `to_end`, highest first. Each run's 64 bytes are all read before any is
/// written.
///
/// # Safety
///
/// `runs` must be at least one, and the `runs * 64` bytes below each end
/// valid.
#[inline(always)]
unsafe fn copy_runs_backward(to_end: *mut u8, from_end: *const u8, runs: usize) {
    // SAFETY: as in `copy_runs_forward`.
    unsafe {
        asm!(
            "2:",
            "ldp {c:q}, {d:q}, [{from}, #-32]!",
            "ldp {a:q}, {b:q}, [{from}, #-32]!",
            "stp {c:q}, {d:q}, [{to}, #-32]!",
            "stp {a:q}, {b:q}, [{to}, #-32]!",
            "subs {runs}, {runs}, #1",
            "b.ne 2b",
            from = inout(reg) from_end => _,
            to = inout(reg) to_end => _,
            runs = inout(reg) runs => _,
            a = out(vreg) _,
            b = out(vreg) _,
            c = out(vreg) _,
            d = out(vreg) _,
            options(nostack),
        );
    }
}

/// Writes `v` over `runs` runs of 64 bytes at `to`.
///
/// # Safety
///
/// `runs` must be at least one, and `to` valid for `runs * 64` bytes.
#[inline(always)]
unsafe fn fill_runs(to: *mut u8, v: uint8x16_t, runs: usize) {
    // SAFETY: the caller vouches for the range and for `runs`.
    unsafe {
        asm!(
            "2:",
            "stp {v:q}, {v:q}, [{to}], #32",
            "stp {v:q}, {v:q}, [{to}], #32",
            "subs {runs}, {runs}, #1",
            "b.ne 2b",
            to = inout(reg) to => _,
            runs = inout(reg) runs => _,
            v = in(vreg) v,
            options(nostack),
        );
    }
}

/// Four bits for each byte of `v`, the first byte lowest: all ones where the
/// byte is 0xff, zero where it is 0. A comparison's answer, as a number whose
/// trailing zeros count four to a byte.
#[inline]
#[target_feature(enable = "neon")]
fn nibbles(v: uint8x16_t) -> u64 {
    let narrowed = vshrn_n_u16::<4>(vreinterpretq_u16_u8(v));
    vget_lane_u64::<0>(vreinterpret_u64_u8(narrowed))
}

/// Where `v` is NUL or `wanted`: the bytes [`find_byte_or_nul`] stops at.
#[inline]
#[target_feature(enable = "neon")]
fn nul_or(v: uint8x16_t, wanted: uint8x16_t) -> uint8x16_t {
    vorrq_u8(vceqq_u8(v, vdupq_n_u8(0)), vceqq_u8(v, wanted))
}

/// The index of the first byte `nibbles` marked. `mask` must not be zero.
#[inline(always)]
const fn first(mask: u64) -> usize {
    (mask.trailing_zeros() / 4) as usize
}

/// The mask of the first `count` bytes' nibbles, for `count` below sixteen.
#[inline(always)]
const fn below(count: usize) -> u64 {
    (1 << (count * 4)) - 1
}

/// Copies up to fifteen bytes, reading all of them before writing any, so
/// that it is right whichever way the two ranges overlap.
///
/// # Safety
///
/// Both must be valid for `n` bytes, and `n` below sixteen.
#[inline(always)]
unsafe fn copy_short(to: *mut u8, from: *const u8, n: usize) {
    if n >= 8 {
        // SAFETY: `n` is at least eight, so both reads are within `from`.
        let head = unsafe { load8(from) };
        // SAFETY: as above.
        let tail = unsafe { load8(from.wrapping_add(n - 8)) };
        // SAFETY: both writes are within `to`, by the same reasoning.
        unsafe { store8(to, head) };
        // SAFETY: as above.
        unsafe { store8(to.wrapping_add(n - 8), tail) };
        return;
    }
    let mut value = 0_u64;
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and the caller vouches for `n` bytes.
        value |= u64::from(unsafe { from.wrapping_add(i).read() }) << (8 * i);
        i += 1;
    }
    i = 0;
    while i < n {
        // SAFETY: as above, for writing.
        unsafe { to.wrapping_add(i).write((value >> (8 * i)) as u8) };
        i += 1;
    }
}

/// Copies `n` bytes from `from` to `to`, lowest first: `memcpy`, and
/// `memmove` when the destination is not above the source.
///
/// Every block is read before a write could reach it: a destination below
/// the source overwrites only source bytes already read, and the last block
/// is read first of all.
///
/// # Safety
///
/// Both must be valid for `n` bytes, and if they overlap, `to` must not be
/// above `from`.
pub(super) unsafe fn copy_forward(to: *mut u8, from: *const u8, n: usize) {
    if n < BLOCK {
        // SAFETY: the caller's contract, with `n` below sixteen.
        unsafe { copy_short(to, from, n) };
        return;
    }
    // SAFETY: `n` is at least sixteen, so the last block is within `from`.
    let tail = unsafe { load(from.wrapping_add(n - BLOCK)) };
    let runs = n / RUN;
    if runs > 0 {
        // SAFETY: `runs * 64` bytes are within both ranges, and the caller's
        // order of the two is the one the loop needs.
        unsafe { copy_runs_forward(to, from, runs) };
    }
    let mut i = runs * RUN;
    // The last whole block is `tail`'s, so stop short of it.
    while n - i > BLOCK {
        // SAFETY: more than a block is left, within both ranges.
        let a = unsafe { load(from.wrapping_add(i)) };
        // SAFETY: as above.
        unsafe { store(to.wrapping_add(i), a) };
        i += BLOCK;
    }
    // SAFETY: the last block of `to`, as `tail` was the last of `from`.
    unsafe { store(to.wrapping_add(n - BLOCK), tail) };
}

/// Copies `n` bytes from `from` to `to`, highest first: `memmove` when the
/// destination is above the source. The mirror of [`copy_forward`]: the
/// first block is read before anything is written, and written last.
///
/// # Safety
///
/// Both must be valid for `n` bytes.
pub(super) unsafe fn copy_backward(to: *mut u8, from: *const u8, n: usize) {
    if n < BLOCK {
        // SAFETY: the caller's contract, with `n` below sixteen.
        unsafe { copy_short(to, from, n) };
        return;
    }
    // SAFETY: `n` is at least sixteen, so the first block is within `from`.
    let head = unsafe { load(from) };
    // Everything at and past `end` is copied.
    let runs = n / RUN;
    if runs > 0 {
        // SAFETY: the `runs * 64` bytes below each end are within both
        // ranges.
        unsafe { copy_runs_backward(to.wrapping_add(n), from.wrapping_add(n), runs) };
    }
    let mut end = n - runs * RUN;
    // The first whole block is `head`'s, so stop short of it.
    while end > BLOCK {
        // SAFETY: the block below `end` starts past zero, within both ranges.
        let a = unsafe { load(from.wrapping_add(end - BLOCK)) };
        // SAFETY: as above.
        unsafe { store(to.wrapping_add(end - BLOCK), a) };
        end -= BLOCK;
    }
    // SAFETY: the first block of `to`, as `head` was the first of `from`.
    unsafe { store(to, head) };
}

/// Sets the `n` bytes at `to` to `byte`.
///
/// # Safety
///
/// `to` must be valid for writing `n` bytes.
#[target_feature(enable = "neon")]
pub(super) unsafe fn fill(to: *mut u8, byte: u8, n: usize) {
    if n < BLOCK {
        if n >= 8 {
            let v = vdup_n_u8(byte);
            // SAFETY: `n` is at least eight, so both writes are within `to`.
            unsafe { store8(to, v) };
            // SAFETY: as above.
            unsafe { store8(to.wrapping_add(n - 8), v) };
            return;
        }
        let mut i = 0;
        while i < n {
            // SAFETY: `i < n`, and the caller vouches for `n` bytes.
            unsafe { to.wrapping_add(i).write(byte) };
            i += 1;
        }
        return;
    }
    let v = vdupq_n_u8(byte);
    let runs = n / RUN;
    if runs > 0 {
        // SAFETY: `runs * 64` bytes are within `to`.
        unsafe { fill_runs(to, v, runs) };
    }
    let mut i = runs * RUN;
    while n - i > BLOCK {
        // SAFETY: more than a block is left.
        unsafe { store(to.wrapping_add(i), v) };
        i += BLOCK;
    }
    // SAFETY: `n` is at least sixteen, so the last block is within `to`.
    unsafe { store(to.wrapping_add(n - BLOCK), v) };
}

/// The difference of the first bytes of `a` and `b` that differ, as unsigned
/// values, or zero: `memcmp`.
///
/// # Safety
///
/// Both must be valid for `n` bytes.
#[target_feature(enable = "neon")]
pub(super) unsafe fn compare(a: *const u8, b: *const u8, n: usize) -> c_int {
    // The byte at `at`'s difference, for an `at` known to differ.
    let differ = |at: usize| -> c_int {
        // SAFETY: `at < n`, and the caller vouches for `n` bytes.
        let x = unsafe { a.wrapping_add(at).read() };
        // SAFETY: as above.
        let y = unsafe { b.wrapping_add(at).read() };
        c_int::from(x) - c_int::from(y)
    };
    if n < BLOCK {
        let mut i = 0;
        while i < n {
            // SAFETY: `i < n`, and the caller vouches for `n` bytes.
            let x = unsafe { a.wrapping_add(i).read() };
            // SAFETY: as above.
            let y = unsafe { b.wrapping_add(i).read() };
            if x != y {
                return c_int::from(x) - c_int::from(y);
            }
            i += 1;
        }
        return 0;
    }
    let mut i = 0;
    // The last block overlaps the one before it; its bytes that were already
    // compared are equal, so the first difference in it is still the first.
    loop {
        let at = if n - i >= BLOCK { i } else { n - BLOCK };
        // SAFETY: `at + 16 <= n`, within both ranges.
        let x = unsafe { load(a.wrapping_add(at)) };
        // SAFETY: as above.
        let y = unsafe { load(b.wrapping_add(at)) };
        let unequal = vmvnq_u8(vceqq_u8(x, y));
        if vmaxvq_u8(unequal) != 0 {
            return differ(at + first(nibbles(unequal)));
        }
        if at + BLOCK >= n {
            return 0;
        }
        i = at + BLOCK;
    }
}

/// The address of the NUL that ends the string at `s`: `strlen`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[target_feature(enable = "neon")]
pub(super) unsafe fn find_nul(s: *const u8) -> *const u8 {
    let zero = vdupq_n_u8(0);
    let offset = s.addr() % BLOCK;
    let mut block = s.wrapping_sub(offset);
    // SAFETY: the block holds `s`'s first byte and is aligned, so it lies in
    // one page; see the module documentation.
    let hits = nibbles(vceqq_u8(unsafe { load(block) }, zero)) >> (offset * 4);
    if hits != 0 {
        return s.wrapping_add(first(hits));
    }
    loop {
        block = block.wrapping_add(BLOCK);
        // SAFETY: no NUL came before `block`, so its first byte is within the
        // string, and it is aligned.
        let hits = vceqq_u8(unsafe { load(block) }, zero);
        if vmaxvq_u8(hits) != 0 {
            return block.wrapping_add(first(nibbles(hits)));
        }
    }
}

/// The address of the first byte of the string at `s` equal to `byte`, or of
/// its NUL: `strchrnul`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[target_feature(enable = "neon")]
pub(super) unsafe fn find_byte_or_nul(s: *const u8, byte: u8) -> *const u8 {
    let wanted = vdupq_n_u8(byte);
    let offset = s.addr() % BLOCK;
    let mut block = s.wrapping_sub(offset);
    // SAFETY: as in `find_nul`.
    let hits = nibbles(nul_or(unsafe { load(block) }, wanted)) >> (offset * 4);
    if hits != 0 {
        return s.wrapping_add(first(hits));
    }
    loop {
        block = block.wrapping_add(BLOCK);
        // SAFETY: as in `find_nul`.
        let hits = nul_or(unsafe { load(block) }, wanted);
        if vmaxvq_u8(hits) != 0 {
            return block.wrapping_add(first(nibbles(hits)));
        }
    }
}

/// The address of the first of the `n` bytes at `s` equal to `byte`, or
/// null: `memchr`.
///
/// # Safety
///
/// `s` must be valid for `n` bytes, or up to the first match.
#[target_feature(enable = "neon")]
pub(super) unsafe fn find_byte(s: *const u8, byte: u8, n: usize) -> *const u8 {
    if n == 0 {
        return core::ptr::null();
    }
    let wanted = vdupq_n_u8(byte);
    let offset = s.addr() % BLOCK;
    let mut block = s.wrapping_sub(offset);
    // The caller's bytes in the first block, from `s`.
    let mine = (BLOCK - offset).min(n);
    // SAFETY: the block holds `s`'s first byte, which `n > 0` makes the
    // caller's, and is aligned; see the module documentation.
    let mut hits = nibbles(vceqq_u8(unsafe { load(block) }, wanted)) >> (offset * 4);
    if mine < BLOCK {
        hits &= below(mine);
    }
    if hits != 0 {
        return s.wrapping_add(first(hits));
    }
    let mut left = n - mine;
    while left >= BLOCK {
        block = block.wrapping_add(BLOCK);
        // SAFETY: the whole aligned block is the caller's.
        let equal = vceqq_u8(unsafe { load(block) }, wanted);
        if vmaxvq_u8(equal) != 0 {
            return block.wrapping_add(first(nibbles(equal)));
        }
        left -= BLOCK;
    }
    if left > 0 {
        block = block.wrapping_add(BLOCK);
        // SAFETY: the block's first byte is the caller's, and it is aligned.
        let hits = nibbles(vceqq_u8(unsafe { load(block) }, wanted)) & below(left);
        if hits != 0 {
            return block.wrapping_add(first(hits));
        }
    }
    core::ptr::null()
}
