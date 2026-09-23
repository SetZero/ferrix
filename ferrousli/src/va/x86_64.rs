//! The x86-64 `va_list`, and its thunk.
//!
//! The System V AMD64 psABI, section 3.5.7, defines `va_list` as an array of
//! one structure:
//!
//! ```text
//! typedef struct {
//!     unsigned int gp_offset;       // next integer register slot, 0..=48
//!     unsigned int fp_offset;       // next vector register slot, 48..=176
//!     void *overflow_arg_area;      // next argument passed on the stack
//!     void *reg_save_area;          // the registers, saved at entry
//! } va_list[1];
//! ```
//!
//! The register save area holds rdi, rsi, rdx, rcx, r8 and r9 at offsets 0 to
//! 40, then xmm0 to xmm7, 16 bytes each, at offsets 48 to 160. An argument is
//! read from the next register slot of its class while one is left, and from
//! the stack after that. Once a class runs out of registers, every later
//! argument of that class is on the stack, in order, mixed with the others
//! that overflowed.
//!
//! Because the type is an array, a `va_list` parameter decays to a pointer to
//! the structure. `vprintf(fmt, ap)` receives a [`VaListTag`] pointer, and
//! reading through it advances the caller's structure in place, as C's
//! `va_arg` does. `va_copy` copies the structure, so both copies share the
//! save area and the stack, which neither writes.
//!
//! # The thunk
//!
//! For a function with `n` named arguments, all of them integers or pointers,
//! the thunk:
//!
//! 1. reserves 200 bytes: the 176-byte register save area and the 24-byte
//!    structure. The call left the stack 8 bytes past a multiple of 16, and 200
//!    is 8 past one too, so the stack is aligned for the call below;
//! 2. saves the six integer registers, and the eight vector registers when
//!    `%al` is non-zero. The caller of a variadic function puts an upper bound
//!    on the number of vector registers used in `%al`, and may leave the upper
//!    registers unset when it is zero;
//! 3. fills the structure as gcc's `va_start` would: `gp_offset` is `8 * n`,
//!    `fp_offset` is 48, the overflow area starts just above the return
//!    address, and the save area is the one just filled;
//! 4. calls the Rust function with the named arguments still in their
//!    registers and the structure's address as argument `n + 1`, and returns
//!    what it returns.
//!
//! The named arguments are not consumed from the save area: `gp_offset` starts
//! past them. A thunk supports up to five named arguments, so that the
//! structure's address still fits in a register.

use core::ffi::c_void;
use core::mem::{offset_of, size_of};

use super::VaList;

/// One element of C's `va_list` on x86-64: where the next argument of each
/// class is. A `va_list` parameter arrives as a pointer to this.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VaListTag {
    /// The offset in the save area of the next integer register, up to 48.
    gp_offset: u32,
    /// The offset in the save area of the next vector register, 48 to 176.
    fp_offset: u32,
    /// The next argument passed on the stack.
    overflow_arg_area: *mut c_void,
    /// Where the thunk or `va_start` saved the argument registers.
    reg_save_area: *mut c_void,
}

const _: () = assert!(size_of::<VaListTag>() == 24);
const _: () = assert!(offset_of!(VaListTag, overflow_arg_area) == 8);
const _: () = assert!(offset_of!(VaListTag, reg_save_area) == 16);

/// A `va_list` as a parameter: the array decays to a pointer to its element.
pub type VaListArg = *mut VaListTag;

/// Where the integer registers end in the save area.
const GP_END: u32 = 48;
/// Where the vector registers end in the save area.
const FP_END: u32 = 176;

/// An x87 extended precision `long double`, as it is stored: a 64-bit
/// significand with an explicit integer bit, then the sign and a 15-bit
/// exponent biased by 16383. The other six bytes of its 16 are padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongDouble {
    /// The significand. Bit 63 is the integer bit.
    pub mantissa: u64,
    /// Bit 15 is the sign, and the rest the biased exponent.
    pub sign_exponent: u16,
}

impl VaList<'_> {
    /// The next argument passed as an integer: an `int`, a `long`, a pointer,
    /// or any narrower integer promoted to `int`. All of them take a whole
    /// 8-byte slot. The high bits of a slot holding an `int` are unspecified,
    /// so the caller truncates to the argument's type.
    ///
    /// # Safety
    ///
    /// The next argument must be of one of those types.
    pub unsafe fn next_word(&mut self) -> u64 {
        let tag = self.tag();
        if tag.gp_offset < GP_END {
            let at = tag
                .reg_save_area
                .wrapping_add(tag.gp_offset as usize)
                .cast::<u64>();
            tag.gp_offset += 8;
            // SAFETY: the offset is inside the save area, where the thunk or
            // `va_start` saved the integer registers.
            return unsafe { at.read_unaligned() };
        }
        let at = tag.overflow_arg_area.cast::<u64>();
        tag.overflow_arg_area = tag.overflow_arg_area.wrapping_add(8);
        // SAFETY: the caller vouches that another argument was passed, and
        // with the registers used up it is on the stack here.
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
        let tag = self.tag();
        if tag.fp_offset < FP_END {
            let at = tag
                .reg_save_area
                .wrapping_add(tag.fp_offset as usize)
                .cast::<f64>();
            tag.fp_offset += 16;
            // SAFETY: the offset is inside the save area, where the vector
            // registers were saved. The caller set `%al`, so they were.
            return unsafe { at.read_unaligned() };
        }
        let at = tag.overflow_arg_area.cast::<f64>();
        tag.overflow_arg_area = tag.overflow_arg_area.wrapping_add(8);
        // SAFETY: as in `next_word`, on the stack.
        unsafe { at.read_unaligned() }
    }

    /// The next argument, which is a `long double`. It is always on the stack,
    /// at a 16-byte boundary, in 16 bytes.
    ///
    /// # Safety
    ///
    /// The next argument must be a `long double`.
    pub unsafe fn next_long_double(&mut self) -> LongDouble {
        let tag = self.tag();
        let area = tag.overflow_arg_area;
        let aligned = area.wrapping_add(area.addr().wrapping_neg() & 15);
        tag.overflow_arg_area = aligned.wrapping_add(16);
        // SAFETY: the caller vouches that a `long double` was passed, and it
        // is in the stack's next 16-byte slot.
        let mantissa = unsafe { aligned.cast::<u64>().read_unaligned() };
        // SAFETY: as above; the sign and exponent follow the significand.
        let sign_exponent = unsafe { aligned.wrapping_add(8).cast::<u16>().read_unaligned() };
        LongDouble {
            mantissa,
            sign_exponent,
        }
    }
}

/// Exports a variadic C function as an entry thunk that builds a `va_list`
/// and calls a Rust function taking it. See the module documentation.
///
/// `variadic!(name, named, target)`: `name` is the C name, `named` the number
/// of named arguments (0 to 5, all integers or pointers), and `target` a
/// function `unsafe extern "C" fn(named..., VaListArg) -> R`.
macro_rules! variadic {
    ($name:ident, $named:tt, $target:path) => {
        #[cfg(not(test))]
        $crate::va::variadic!(@thunk stringify!($name), $named, $target);
        #[cfg(test)]
        $crate::va::variadic!(@thunk concat!("ferrousli_test_", stringify!($name)), $named, $target);
    };
    (@register 0) => { "%rdi" };
    (@register 1) => { "%rsi" };
    (@register 2) => { "%rdx" };
    (@register 3) => { "%rcx" };
    (@register 4) => { "%r8" };
    (@register 5) => { "%r9" };
    (@thunk $label:expr, $named:tt, $target:path) => {
        core::arch::global_asm!(
            concat!(".pushsection .text.ferrousli_va.", $label, ",\"ax\",@progbits"),
            ".p2align 4",
            concat!(".globl ", $label),
            concat!(".type ", $label, ",@function"),
            concat!($label, ":"),
            // The save area at 0(%rsp), the structure at 176(%rsp), the
            // return address at 200(%rsp), and the stack arguments above.
            "sub $200, %rsp",
            "mov %rdi, 0(%rsp)",
            "mov %rsi, 8(%rsp)",
            "mov %rdx, 16(%rsp)",
            "mov %rcx, 24(%rsp)",
            "mov %r8, 32(%rsp)",
            "mov %r9, 40(%rsp)",
            "test %al, %al",
            concat!("je .Lva_", $label, "_saved"),
            "movaps %xmm0, 48(%rsp)",
            "movaps %xmm1, 64(%rsp)",
            "movaps %xmm2, 80(%rsp)",
            "movaps %xmm3, 96(%rsp)",
            "movaps %xmm4, 112(%rsp)",
            "movaps %xmm5, 128(%rsp)",
            "movaps %xmm6, 144(%rsp)",
            "movaps %xmm7, 160(%rsp)",
            concat!(".Lva_", $label, "_saved:"),
            "movl ${gp}, 176(%rsp)",
            "movl $48, 180(%rsp)",
            "lea 208(%rsp), %rax",
            "mov %rax, 184(%rsp)",
            "mov %rsp, 192(%rsp)",
            "lea 176(%rsp), %rax",
            concat!("mov %rax, ", $crate::va::variadic!(@register $named)),
            "call {target}",
            "add $200, %rsp",
            "ret",
            concat!(".size ", $label, ",.-", $label),
            ".popsection",
            gp = const 8 * $named,
            target = sym $target,
            options(att_syntax),
        );
    };
}

pub(crate) use variadic;
