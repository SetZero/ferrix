//! The AArch64 `va_list`, and its thunk.
//!
//! The Procedure Call Standard for the Arm 64-bit Architecture (AAPCS64),
//! appendix B, defines `va_list` as a structure:
//!
//! ```text
//! typedef struct {
//!     void *__stack;    // next argument passed on the stack
//!     void *__gr_top;   // the end of the saved general registers
//!     void *__vr_top;   // the end of the saved vector registers
//!     int   __gr_offs;  // -8 * general registers left, from __gr_top
//!     int   __vr_offs;  // -16 * vector registers left, from __vr_top
//! } va_list;
//! ```
//!
//! An argument is read from `top + offs` while `offs` is negative, and the
//! offset then moves up one slot: 8 bytes for x0 to x7, 16 for q0 to q7. Once
//! it reaches zero every later argument of that class is on the stack, in
//! 8-byte slots, or 16-byte aligned ones for a 16-byte type.
//!
//! The structure is 32 bytes, so it is passed as a pointer to a copy the
//! caller made (AAPCS64 B.4): `vprintf(fmt, ap)` receives a [`VaListTag`]
//! pointer just as on x86-64, though advancing it does not move the caller's
//! list, which C leaves indeterminate after the call anyway.
//!
//! A `long double` is IEEE binary128, and travels in a vector register like a
//! `double` does.
//!
//! # The thunk
//!
//! For a function with `n` named arguments, all of them integers or pointers,
//! the thunk:
//!
//! 1. pushes a frame record, then reserves 224 bytes: q0 to q7 at 0, x0 to x7
//!    at 128, and the structure at 192. Every register is saved, since there is
//!    no count of the vector registers used as x86-64's `%al` gives;
//! 2. fills the structure: the stack arguments start where the stack pointer
//!    was on entry, `__gr_top` is the end of the general registers and
//!    `__vr_top` the end of the vector ones, and `__gr_offs` skips the `n`
//!    named arguments;
//! 3. calls the Rust function with the named arguments still in their
//!    registers and the structure's address in x`n`.
//!
//! Up to seven named arguments fit.

use core::ffi::c_void;
use core::mem::{offset_of, size_of};

use super::VaList;

/// C's `va_list` on AArch64. A parameter arrives as a pointer to a copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VaListTag {
    /// The next argument passed on the stack.
    stack: *mut c_void,
    /// The end of the general registers' save area.
    gr_top: *mut c_void,
    /// The end of the vector registers' save area.
    vr_top: *mut c_void,
    /// Minus the bytes of general registers left: -64 to 0.
    gr_offs: i32,
    /// Minus the bytes of vector registers left: -128 to 0.
    vr_offs: i32,
}

const _: () = assert!(size_of::<VaListTag>() == 32);
const _: () = assert!(offset_of!(VaListTag, gr_offs) == 24);
const _: () = assert!(offset_of!(VaListTag, vr_offs) == 28);

/// A `va_list` as a parameter: a pointer to the caller's copy.
pub type VaListArg = *mut VaListTag;

/// An IEEE binary128 `long double`, as its bits: the sign at bit 127, a
/// 15-bit exponent biased by 16383, and a 112-bit fraction with the leading
/// one implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongDouble {
    /// The value's bits.
    pub bits: u128,
}

impl VaList<'_> {
    /// Takes the next general register slot or stack slot, and returns its
    /// address.
    fn next_general(&mut self) -> *const u64 {
        let tag = self.tag();
        if tag.gr_offs < 0 {
            let at = tag.gr_top.wrapping_offset(tag.gr_offs as isize);
            tag.gr_offs += 8;
            return at.cast();
        }
        let at = tag.stack;
        tag.stack = tag.stack.wrapping_add(8);
        at.cast()
    }

    /// Takes the next vector register slot, or a stack slot of `size` bytes
    /// at a `size` boundary, and returns its address.
    fn next_vector(&mut self, size: usize) -> *const u8 {
        let tag = self.tag();
        if tag.vr_offs < 0 {
            let at = tag.vr_top.wrapping_offset(tag.vr_offs as isize);
            tag.vr_offs += 16;
            return at.cast();
        }
        let area = tag.stack;
        let aligned = area.wrapping_add(area.addr().wrapping_neg() & (size - 1));
        tag.stack = aligned.wrapping_add(size);
        aligned.cast()
    }

    /// The next argument passed as an integer: an `int`, a `long`, a pointer,
    /// or any narrower integer promoted to `int`. All of them take a whole
    /// 8-byte slot. The high bits of a slot holding an `int` are unspecified,
    /// so the caller truncates to the argument's type.
    ///
    /// # Safety
    ///
    /// The next argument must be of one of those types.
    pub unsafe fn next_word(&mut self) -> u64 {
        let at = self.next_general();
        // SAFETY: the slot is in the save area or on the stack, where the
        // caller vouches another argument was passed.
        unsafe { at.read_unaligned() }
    }

    /// The next argument, which is a `long long`: here the same slot as any
    /// other integer.
    ///
    /// # Safety
    ///
    /// The next argument must be a 64-bit integer.
    pub unsafe fn next_wide(&mut self) -> u64 {
        // SAFETY: the caller vouches for the argument's type.
        unsafe { self.next_word() }
    }

    /// The next argument, which is a `double`, or a `float` promoted to one.
    ///
    /// # Safety
    ///
    /// The next argument must be a `double`.
    pub unsafe fn next_double(&mut self) -> f64 {
        let at = self.next_vector(8);
        // SAFETY: as in `next_word`. A register slot holds the value in its
        // low eight bytes.
        unsafe { at.cast::<f64>().read_unaligned() }
    }

    /// The next argument, which is a `long double`.
    ///
    /// # Safety
    ///
    /// The next argument must be a `long double`.
    pub unsafe fn next_long_double(&mut self) -> LongDouble {
        let at = self.next_vector(16);
        // SAFETY: as in `next_double`; the value fills the whole slot.
        let bits = unsafe { at.cast::<u128>().read_unaligned() };
        LongDouble { bits }
    }
}

/// Exports a variadic C function as an entry thunk that builds a `va_list`
/// and calls a Rust function taking it. See the module documentation.
///
/// `variadic!(name, named, target)`: `name` is the C name, `named` the number
/// of named arguments (0 to 7, all integers or pointers), and `target` a
/// function `unsafe extern "C" fn(named..., VaListArg) -> R`.
macro_rules! variadic {
    ($name:ident, $named:tt, $target:path) => {
        #[cfg(not(test))]
        $crate::va::variadic!(@thunk stringify!($name), $named, $target);
        #[cfg(test)]
        $crate::va::variadic!(@thunk concat!("ferrousli_test_", stringify!($name)), $named, $target);
    };
    (@thunk $label:expr, $named:tt, $target:path) => {
        core::arch::global_asm!(
            concat!(".pushsection .text.ferrousli_va.", $label, ",\"ax\",@progbits"),
            ".p2align 2",
            concat!(".globl ", $label),
            concat!(".type ", $label, ",@function"),
            concat!($label, ":"),
            "stp x29, x30, [sp, #-16]!",
            "mov x29, sp",
            "sub sp, sp, #224",
            "stp q0, q1, [sp, #0]",
            "stp q2, q3, [sp, #32]",
            "stp q4, q5, [sp, #64]",
            "stp q6, q7, [sp, #96]",
            "stp x0, x1, [sp, #128]",
            "stp x2, x3, [sp, #144]",
            "stp x4, x5, [sp, #160]",
            "stp x6, x7, [sp, #176]",
            // __stack: where the stack pointer was on entry.
            "add x9, x29, #16",
            "str x9, [sp, #192]",
            // __gr_top and __vr_top: the ends of the two save areas.
            "add x9, sp, #192",
            "str x9, [sp, #200]",
            "add x9, sp, #128",
            "str x9, [sp, #208]",
            "mov w9, #{gr_offs}",
            "str w9, [sp, #216]",
            "mov w9, #-128",
            "str w9, [sp, #220]",
            concat!("add x", stringify!($named), ", sp, #192"),
            "bl {target}",
            "mov sp, x29",
            "ldp x29, x30, [sp], #16",
            "ret",
            concat!(".size ", $label, ",.-", $label),
            ".popsection",
            gr_offs = const 8 * $named - 64,
            target = sym $target,
        );
    };
}

pub(crate) use variadic;
