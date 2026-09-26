//! The ARMv7-A `va_list`, and its thunk.
//!
//! The Procedure Call Standard for the Arm Architecture (AAPCS), section 8.1.4,
//! makes `va_list` a structure holding one pointer, `void *__ap`, to the next
//! argument. That works because a variadic call uses the base standard even
//! under the hard-float variant: every argument, a `double` included, goes in
//! r0 to r3 and then on the stack, never in a VFP register. Saving r0 to r3
//! just below the stack arguments makes all of them one array, which a cursor
//! walks: 4 bytes for an `int`, a `long` or a pointer, and 8 bytes at an
//! 8-byte boundary for a `long long` or a `double`. The boundary is the same
//! one the caller used, since the saved registers start 8-aligned and a 64-bit
//! argument never starts in an odd register.
//!
//! The structure is four bytes, so a `va_list` parameter is passed by value in
//! a register: `vprintf(fmt, ap)` receives the pointer itself. [`VaListArg`]
//! is therefore [`VaListTag`], not a pointer to one.
//!
//! A `long double` is a `double` here.
//!
//! # The thunk
//!
//! For a function with `n` named arguments, all of them integers or pointers,
//! the thunk pushes r0 to r3, then r4 and lr and eight bytes for outgoing
//! arguments, keeping the stack 8-aligned. The list starts `4 * n` bytes into
//! the saved registers. It is passed in r`n` when `n` is 3 or less and on the
//! stack otherwise; a fifth named argument, which the caller put on the stack,
//! is copied down beside it. Up to five named arguments fit.

use core::ffi::c_void;
use core::mem::size_of;

use super::VaList;

/// C's `va_list` on ARMv7-A: the address of the next argument.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VaListTag {
    /// The next argument.
    ap: *mut c_void,
}

const _: () = assert!(size_of::<VaListTag>() == 4);

/// A `va_list` as a parameter: the structure itself, in a register.
pub type VaListArg = VaListTag;

/// A `long double`, which is a `double` on ARMv7-A.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongDouble(pub f64);

impl VaList<'_> {
    /// Takes the next `size` bytes at a `size` boundary, and returns their
    /// address.
    fn next_slot(&mut self, size: usize) -> *const u8 {
        let tag = self.tag();
        let at = tag
            .ap
            .wrapping_add(tag.ap.addr().wrapping_neg() & (size - 1));
        tag.ap = at.wrapping_add(size);
        at.cast()
    }

    /// The next argument passed as a 32-bit integer: an `int`, a `long`, a
    /// pointer, or any narrower integer promoted to `int`, zero-extended.
    ///
    /// # Safety
    ///
    /// The next argument must be of one of those types.
    pub unsafe fn next_word(&mut self) -> u64 {
        let at = self.next_slot(4);
        // SAFETY: the caller vouches another argument was passed, so the slot
        // is in the saved registers or on the stack.
        u64::from(unsafe { at.cast::<u32>().read_unaligned() })
    }

    /// The next argument, which is a `long long` or `unsigned long long`.
    ///
    /// # Safety
    ///
    /// The next argument must be a 64-bit integer.
    pub unsafe fn next_wide(&mut self) -> u64 {
        let at = self.next_slot(8);
        // SAFETY: as in `next_word`.
        unsafe { at.cast::<u64>().read_unaligned() }
    }

    /// The next argument, which is a `double`, or a `float` promoted to one.
    ///
    /// # Safety
    ///
    /// The next argument must be a `double`.
    pub unsafe fn next_double(&mut self) -> f64 {
        let at = self.next_slot(8);
        // SAFETY: as in `next_word`.
        unsafe { at.cast::<f64>().read_unaligned() }
    }

    /// The next argument, which is a `long double`, the same as a `double`.
    ///
    /// # Safety
    ///
    /// The next argument must be a `long double`.
    pub unsafe fn next_long_double(&mut self) -> LongDouble {
        // SAFETY: the caller vouches for the argument's type.
        LongDouble(unsafe { self.next_double() })
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
    // After the pushes, the saved r0 is at sp + 16 and the caller's stack
    // arguments at sp + 32; the list is in r12.
    (@pass 0) => { "mov r0, r12" };
    (@pass 1) => { "mov r1, r12" };
    (@pass 2) => { "mov r2, r12" };
    (@pass 3) => { "mov r3, r12" };
    (@pass 4) => { "str r12, [sp]" };
    (@pass 5) => { "ldr r4, [sp, #32]\n str r4, [sp]\n str r12, [sp, #4]" };
    (@thunk $label:expr, $named:tt, $target:path) => {
        core::arch::global_asm!(
            concat!(".pushsection .text.ferrousli_va.", $label, ",\"ax\",%progbits"),
            ".p2align 2",
            ".arm",
            concat!(".globl ", $label),
            concat!(".type ", $label, ",%function"),
            concat!($label, ":"),
            "push {{r0-r3}}",
            "push {{r4, lr}}",
            "sub sp, sp, #8",
            "add r12, sp, #{start}",
            $crate::va::variadic!(@pass $named),
            "bl {target}",
            "add sp, sp, #8",
            "pop {{r4, lr}}",
            "add sp, sp, #16",
            "bx lr",
            concat!(".size ", $label, ",.-", $label),
            ".popsection",
            start = const 16 + 4 * $named,
            target = sym $target,
        );
    };
}

pub(crate) use variadic;
