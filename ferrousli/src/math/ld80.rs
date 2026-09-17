//! The x87 80-bit extended `long double`: [`F80`], a value Rust can hold,
//! the x87 instructions the `long double` functions compute with, and the
//! shims that pass one to and from C.
//!
//! See [`crate::math`]'s `long double` section for why the arithmetic is x87
//! instructions rather than Rust operators.
//!
//! # The format
//!
//! Ten bytes, little-endian: a 64-bit significand whose top bit is an
//! explicit integer bit, then a word holding the sign in bit 15 and a 15-bit
//! exponent biased by 16383. musl's `union ldshape` names the two parts `m`
//! and `se`; [`F80::mantissa`] and [`F80::sign_exponent`] read them.
//!
//! # Calling convention
//!
//! The SysV ABI passes a `long double` argument in memory: 16 bytes in the
//! caller's frame, above the return address, in the order of the arguments
//! and after those passed in registers take their registers. It returns one
//! in `st(0)`. Neither is anything a Rust signature can say, so each exported
//! function is a naked shim that [`export`] writes: it passes the addresses
//! of the arguments' stack slots to a Rust `extern "C"` adapter, which reads
//! them as [`F80`]s and calls the function's Rust body, and then loads the
//! result with `fld`. An integer, pointer, `float` or `double` travels in its
//! register as usual.
//!
//! # x87 state
//!
//! Every operation here loads its operands from memory, runs its
//! instructions, and stores the result back, leaving the x87 stack as empty as
//! it found it, which is how the ABI has it between calls. Each `asm!` block
//! still declares all eight x87 registers clobbered, as Rust requires of a
//! block that uses them. Nothing changes the control word except
//! [`F80::rndint_with_control`] and [`F80::to_i64_truncating`], which restore
//! it.

use core::arch::asm;
use core::ffi::c_int;

use crate::math::classify::{FP_NAN, classify_x87};

pub mod manipulate;
pub mod remainder;
pub mod rounding;
pub mod sqrt;

/// An `asm!` block of x87 instructions, with every x87 register declared
/// clobbered, and without touching the stack. Templates, then `;`, then the
/// operands, each followed by a comma.
macro_rules! x87 {
    ($($template:literal),+ ; $($operands:tt)*) => {
        asm!(
            $($template),+,
            $($operands)*
            out("st(0)") _,
            out("st(1)") _,
            out("st(2)") _,
            out("st(3)") _,
            out("st(4)") _,
            out("st(5)") _,
            out("st(6)") _,
            out("st(7)") _,
            options(nostack),
        )
    };
}

/// An x87 `long double`: its ten bytes, significand first.
///
/// Every encoding is a value of this type, including those the x87 refuses
/// as operands: unnormals, pseudo-infinities and pseudo-NaNs. The arithmetic
/// raises invalid for them and returns the default NaN, as the hardware
/// does.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct F80 {
    /// The significand, then the sign and exponent, little-endian.
    bytes: [u8; 10],
}

/// Defines an operation that loads two operands and applies one x87
/// instruction to them, `st(0)` holding the first and `st(1)` the second.
macro_rules! binary {
    ($(#[$doc:meta])* $name:ident, $instruction:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(self, other: Self) -> Self {
            let mut result = Self::ZERO;
            // SAFETY: the instructions read ten bytes from each operand and
            // write ten to `result`, all locals, and leave the x87 stack empty.
            unsafe {
                x87!(
                    "fld tbyte ptr [{b}]",
                    "fld tbyte ptr [{a}]",
                    $instruction,
                    "fstp tbyte ptr [{r}]",
                    "fstp st(0)";
                    a = in(reg) &raw const self,
                    b = in(reg) &raw const other,
                    r = in(reg) &raw mut result,
                );
            }
            result
        }
    };
}

/// Defines an operation that loads one operand and applies one x87
/// instruction to it.
macro_rules! unary {
    ($(#[$doc:meta])* $name:ident, $instruction:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(self) -> Self {
            let mut result = Self::ZERO;
            // SAFETY: the instructions read ten bytes from the operand and
            // write ten to `result`, both locals, and leave the x87 stack
            // empty.
            unsafe {
                x87!(
                    "fld tbyte ptr [{a}]",
                    $instruction,
                    "fstp tbyte ptr [{r}]";
                    a = in(reg) &raw const self,
                    r = in(reg) &raw mut result,
                );
            }
            result
        }
    };
}

/// Defines a comparison of two operands with `st(0)` holding the first,
/// returning the flags it sets.
macro_rules! compare {
    ($(#[$doc:meta])* $name:ident, $instruction:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(a: Self, b: Self) -> Flags {
            let zero: u8;
            let parity: u8;
            let carry: u8;
            // SAFETY: the instructions read ten bytes from each operand, both
            // locals, set three byte registers from the flags, and leave the
            // x87 stack empty; `fstp` leaves the flags alone.
            unsafe {
                x87!(
                    "fld tbyte ptr [{b}]",
                    "fld tbyte ptr [{a}]",
                    $instruction,
                    "setz {z}",
                    "setp {p}",
                    "setb {c}",
                    "fstp st(0)";
                    a = in(reg) &raw const a,
                    b = in(reg) &raw const b,
                    z = out(reg_byte) zero,
                    p = out(reg_byte) parity,
                    c = out(reg_byte) carry,
                );
            }
            Flags {
                zero: zero != 0,
                parity: parity != 0,
                carry: carry != 0,
            }
        }
    };
}

impl F80 {
    /// +0.
    pub(crate) const ZERO: Self = Self::from_parts(0, 0);
    /// 1.
    pub(crate) const ONE: Self = Self::from_parts(1 << 63, 0x3fff);
    /// The quiet NaN C's `NAN` converts to: positive, with only the quiet bit
    /// set beside the integer bit.
    pub(crate) const NAN: Self = Self::from_parts(0xc000_0000_0000_0000, 0x7fff);

    /// The value with this significand, whose bit 63 is the integer bit, and
    /// this sign-and-exponent word.
    #[inline]
    pub(crate) const fn from_parts(mantissa: u64, sign_exponent: u16) -> Self {
        let [m0, m1, m2, m3, m4, m5, m6, m7] = mantissa.to_le_bytes();
        let [s0, s1] = sign_exponent.to_le_bytes();
        Self {
            bytes: [m0, m1, m2, m3, m4, m5, m6, m7, s0, s1],
        }
    }

    /// The significand, integer bit included: musl's `u.i.m`.
    #[inline]
    pub(crate) const fn mantissa(self) -> u64 {
        let [m0, m1, m2, m3, m4, m5, m6, m7, _, _] = self.bytes;
        u64::from_le_bytes([m0, m1, m2, m3, m4, m5, m6, m7])
    }

    /// The sign and the biased exponent: musl's `u.i.se`.
    #[inline]
    pub(crate) const fn sign_exponent(self) -> u16 {
        let [_, _, _, _, _, _, _, _, s0, s1] = self.bytes;
        u16::from_le_bytes([s0, s1])
    }

    /// The biased exponent, `se & 0x7fff`.
    #[inline]
    pub(crate) const fn exponent(self) -> u16 {
        self.sign_exponent() & 0x7fff
    }

    /// Whether the sign bit is set.
    #[inline]
    pub(crate) const fn is_negative(self) -> bool {
        self.sign_exponent() >> 15 != 0
    }

    /// `fpclassify`'s class, as musl's `__fpclassifyl` gives it: the encodings
    /// the x87 refuses are NaNs.
    #[inline]
    pub(crate) const fn class(self) -> c_int {
        classify_x87(self.mantissa(), self.sign_exponent())
    }

    /// C's `isnan`, which for `long double` calls `__fpclassifyl`.
    #[inline]
    pub(crate) const fn is_nan(self) -> bool {
        self.class() == FP_NAN
    }

    binary!(
        /// `self + other`, with `fadd`, rounded in the current mode.
        add, "fadd st(0), st(1)"
    );
    binary!(
        /// `self - other`, with `fsub`, rounded in the current mode.
        sub, "fsub st(0), st(1)"
    );
    binary!(
        /// `self × other`, with `fmul`, rounded in the current mode.
        mul, "fmul st(0), st(1)"
    );
    binary!(
        /// `self / other`, with `fdiv`, rounded in the current mode.
        div, "fdiv st(0), st(1)"
    );
    unary!(
        /// `-self`, with `fchs`, which raises nothing.
        neg, "fchs"
    );
    unary!(
        /// `|self|`, with `fabs`, which raises nothing.
        abs, "fabs"
    );
    unary!(
        /// The square root, with `fsqrt`: correctly rounded, inexact when it
        /// rounds, invalid for a negative number.
        sqrt, "fsqrt"
    );
    unary!(
        /// `self` rounded to an integer in the current mode, with `frndint`,
        /// raising inexact when that changes it.
        rndint, "frndint"
    );
    compare!(
        /// `fcomi`, comparing `a` with `b`: invalid for any NaN.
        fcomi, "fcomip st(0), st(1)"
    );
    compare!(
        /// `fucomi`, comparing `a` with `b`: invalid only for a signalling
        /// NaN or an encoding the x87 refuses.
        fucomi, "fucomip st(0), st(1)"
    );

    /// `self` rounded to an integer with the control word's high byte set to
    /// `high` for the one `frndint`, as musl's `x86_64/floorl.s` does: 0x07
    /// rounds down, 0x0b up and 0x0f toward zero, each with 64-bit precision.
    #[inline]
    pub(crate) fn rndint_with_control(self, high: u8) -> Self {
        let mut control = 0u16;
        // SAFETY: `fnstcw` writes two bytes to a local.
        unsafe {
            x87!("fnstcw word ptr [{c}]"; c = in(reg) &raw mut control,);
        }
        let rounding = control & 0xff | u16::from(high) << 8;
        let mut result = Self::ZERO;
        // SAFETY: the instructions read the operand and two control words,
        // all locals, write ten bytes to `result`, leave the x87 stack empty,
        // and load the saved control word again.
        unsafe {
            x87!(
                "fld tbyte ptr [{a}]",
                "fldcw word ptr [{rounding}]",
                "frndint",
                "fldcw word ptr [{control}]",
                "fstp tbyte ptr [{r}]";
                a = in(reg) &raw const self,
                rounding = in(reg) &raw const rounding,
                control = in(reg) &raw const control,
                r = in(reg) &raw mut result,
            );
        }
        result
    }

    /// The partial remainder of `self` by `divisor`, repeated until complete,
    /// as musl's `x86_64/fmodl.c` runs `fprem`: exact, with the sign of
    /// `self`, invalid for a zero divisor, an infinite dividend or a NaN.
    #[inline]
    pub(crate) fn fprem(self, divisor: Self) -> Self {
        self.partial_remainder(divisor, false).0
    }

    /// As [`F80::fprem`] with `fprem1`, the IEEE remainder, rounding the
    /// quotient to nearest. The status word after the last `fprem1` comes
    /// back too: its C0, C3 and C1 carry the quotient's low three bits.
    #[inline]
    pub(crate) fn fprem1(self, divisor: Self) -> (Self, u16) {
        self.partial_remainder(divisor, true)
    }

    /// [`F80::fprem`] or [`F80::fprem1`], and the final status word.
    #[inline]
    fn partial_remainder(self, divisor: Self, ieee: bool) -> (Self, u16) {
        let mut result = Self::ZERO;
        let status: u16;
        if ieee {
            // SAFETY: the instructions read ten bytes from each operand and
            // write ten to `result`, all locals, and leave the x87 stack
            // empty. The loop ends when C2 is clear, which the x87 guarantees
            // after at most one pass per 63 bits of exponent difference.
            unsafe {
                x87!(
                    "fld tbyte ptr [{y}]",
                    "fld tbyte ptr [{x}]",
                    "2:",
                    "fprem1",
                    "fnstsw ax",
                    "test ah, 4",
                    "jnz 2b",
                    "fstp tbyte ptr [{r}]",
                    "fstp st(0)";
                    x = in(reg) &raw const self,
                    y = in(reg) &raw const divisor,
                    r = in(reg) &raw mut result,
                    out("ax") status,
                );
            }
        } else {
            // SAFETY: as above, with `fprem`.
            unsafe {
                x87!(
                    "fld tbyte ptr [{y}]",
                    "fld tbyte ptr [{x}]",
                    "2:",
                    "fprem",
                    "fnstsw ax",
                    "test ah, 4",
                    "jnz 2b",
                    "fstp tbyte ptr [{r}]",
                    "fstp st(0)";
                    x = in(reg) &raw const self,
                    y = in(reg) &raw const divisor,
                    r = in(reg) &raw mut result,
                    out("ax") status,
                );
            }
        }
        (result, status)
    }

    /// `self` converted to a 64-bit integer by `fistp`, rounding in the
    /// current mode: inexact if that rounds, and invalid with `i64::MIN` for
    /// a NaN or a value out of range.
    #[inline]
    pub(crate) fn to_i64(self) -> i64 {
        let mut result = 0i64;
        // SAFETY: the instructions read the operand and write eight bytes to
        // `result`, both locals, and leave the x87 stack empty.
        unsafe {
            x87!(
                "fld tbyte ptr [{a}]",
                "fistp qword ptr [{r}]";
                a = in(reg) &raw const self,
                r = in(reg) &raw mut result,
            );
        }
        result
    }

    /// [`F80::to_i64`] rounding toward zero, as GCC converts a `long double`
    /// to an integer: the control word's rounding bits are set for the one
    /// `fistp`, as GCC's code sets them, and restored.
    #[inline]
    pub(crate) fn to_i64_truncating(self) -> i64 {
        let mut result = 0i64;
        let mut control = [0u16; 2];
        // SAFETY: the instructions read the operand, keep two control words
        // in a local array, write eight bytes to `result`, leave the x87
        // stack empty and load the saved control word again.
        unsafe {
            x87!(
                "fld tbyte ptr [{a}]",
                "fnstcw word ptr [{c}]",
                "movzx eax, word ptr [{c}]",
                "or ah, 12",
                "mov word ptr [{c} + 2], ax",
                "fldcw word ptr [{c} + 2]",
                "fistp qword ptr [{r}]",
                "fldcw word ptr [{c}]";
                a = in(reg) &raw const self,
                c = in(reg) &raw mut control,
                r = in(reg) &raw mut result,
                out("ax") _,
            );
        }
        result
    }

    /// The `int` `n`, loaded with `fild`, which is exact.
    #[inline]
    pub(crate) fn from_i32(n: i32) -> Self {
        let mut result = Self::ZERO;
        // SAFETY: the instructions read four bytes from `n` and write ten to
        // `result`, both locals, and leave the x87 stack empty.
        unsafe {
            x87!(
                "fild dword ptr [{n}]",
                "fstp tbyte ptr [{r}]";
                n = in(reg) &raw const n,
                r = in(reg) &raw mut result,
            );
        }
        result
    }

    /// The `double` `x`, loaded with `fld`, as C converts it: exact, but
    /// raising denormal for a subnormal and invalid for a signalling NaN,
    /// which it quiets.
    #[inline]
    pub(crate) fn from_f64(x: f64) -> Self {
        let mut result = Self::ZERO;
        // SAFETY: the instructions read eight bytes from `x` and write ten to
        // `result`, both locals, and leave the x87 stack empty.
        unsafe {
            x87!(
                "fld qword ptr [{x}]",
                "fstp tbyte ptr [{r}]";
                x = in(reg) &raw const x,
                r = in(reg) &raw mut result,
            );
        }
        result
    }

    /// [`F80::from_f64`] for `float`.
    #[inline]
    pub(crate) fn from_f32(x: f32) -> Self {
        let mut result = Self::ZERO;
        // SAFETY: as in `from_f64`, with four bytes.
        unsafe {
            x87!(
                "fld dword ptr [{x}]",
                "fstp tbyte ptr [{r}]";
                x = in(reg) &raw const x,
                r = in(reg) &raw mut result,
            );
        }
        result
    }

    /// `self` as a `double`, stored with `fstp`, as C converts it: rounded in
    /// the current mode, raising inexact, overflow and underflow as that
    /// needs.
    #[inline]
    pub(crate) fn to_f64(self) -> f64 {
        let mut result = 0.0f64;
        // SAFETY: the instructions read the operand and write eight bytes to
        // `result`, both locals, and leave the x87 stack empty.
        unsafe {
            x87!(
                "fld tbyte ptr [{a}]",
                "fstp qword ptr [{r}]";
                a = in(reg) &raw const self,
                r = in(reg) &raw mut result,
            );
        }
        result
    }

    /// [`F80::to_f64`] for `float`.
    #[inline]
    pub(crate) fn to_f32(self) -> f32 {
        let mut result = 0.0f32;
        // SAFETY: as in `to_f64`, with four bytes.
        unsafe {
            x87!(
                "fld tbyte ptr [{a}]",
                "fstp dword ptr [{r}]";
                a = in(reg) &raw const self,
                r = in(reg) &raw mut result,
            );
        }
        result
    }

    /// `self + x` for a `double` `x`, with `fadd qword ptr`, as GCC adds a
    /// `double` to a `long double` in memory. Converting `x` first would not
    /// be the same: where `x` is a signalling NaN and `self` a quiet one,
    /// `fadd` returns `self`, but a converted `x` is quiet and may win.
    #[inline]
    pub(crate) fn add_f64(self, x: f64) -> Self {
        let mut result = Self::ZERO;
        // SAFETY: the instructions read the operands and write ten bytes to
        // `result`, all locals, and leave the x87 stack empty.
        unsafe {
            x87!(
                "fld tbyte ptr [{a}]",
                "fadd qword ptr [{x}]",
                "fstp tbyte ptr [{r}]";
                a = in(reg) &raw const self,
                x = in(reg) &raw const x,
                r = in(reg) &raw mut result,
            );
        }
        result
    }

    /// [`F80::add_f64`] for `float`, with `fadd dword ptr`.
    #[inline]
    pub(crate) fn add_f32(self, x: f32) -> Self {
        let mut result = Self::ZERO;
        // SAFETY: as in `add_f64`, with a four-byte operand.
        unsafe {
            x87!(
                "fld tbyte ptr [{a}]",
                "fadd dword ptr [{x}]",
                "fstp tbyte ptr [{r}]";
                a = in(reg) &raw const self,
                x = in(reg) &raw const x,
                r = in(reg) &raw mut result,
            );
        }
        result
    }
}

/// The flags an x87 comparison of `a` with `b` leaves, read as the jumps
/// after it read them. An unordered result sets all three.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Flags {
    /// ZF: equal, or unordered.
    zero: bool,
    /// PF: unordered.
    parity: bool,
    /// CF: `a` below `b`, or unordered.
    carry: bool,
}

impl Flags {
    /// `ja`: `a > b`, ordered.
    #[inline]
    pub(crate) const fn above(self) -> bool {
        !self.carry && !self.zero
    }

    /// `jb`: `a < b`, or unordered.
    #[inline]
    pub(crate) const fn below(self) -> bool {
        self.carry
    }

    /// `jbe`: `a <= b`, or unordered.
    #[inline]
    pub(crate) const fn below_or_equal(self) -> bool {
        self.carry || self.zero
    }

    /// `jp` not taken, then `je`: `a == b`, ordered.
    #[inline]
    pub(crate) const fn equal(self) -> bool {
        self.zero && !self.parity
    }
}

/// Parses a C hexadecimal floating constant with an optional `L` suffix,
/// such as `-0x1.62e42fefa39ef358p-1L`, into the `long double` it names
/// exactly, or `None` if it names none or is anything else.
pub(crate) const fn parse_hex(text: &str) -> Option<F80> {
    let mut rest = text.as_bytes();
    let mut negative = false;
    if let [sign @ (b'-' | b'+'), tail @ ..] = rest {
        negative = *sign == b'-';
        rest = tail;
    }
    match rest {
        [b'0', b'x' | b'X', tail @ ..] => rest = tail,
        _ => return None,
    }
    let mut mantissa = 0u128;
    let mut exponent = 0i32;
    let mut digits = 0;
    let mut point = false;
    while let [c, tail @ ..] = rest {
        if *c == b'.' && !point {
            point = true;
        } else {
            let digit = match *c {
                b'0'..=b'9' => *c - b'0',
                b'a'..=b'f' => *c - b'a' + 10,
                b'A'..=b'F' => *c - b'A' + 10,
                _ => break,
            };
            if mantissa >> 124 != 0 {
                return None;
            }
            mantissa = mantissa << 4 | digit as u128;
            if point {
                exponent -= 4;
            }
            digits += 1;
        }
        rest = tail;
    }
    if digits == 0 {
        return None;
    }
    match rest {
        [b'p' | b'P', tail @ ..] => rest = tail,
        _ => return None,
    }
    let mut exponent_negative = false;
    if let [sign @ (b'-' | b'+'), tail @ ..] = rest {
        exponent_negative = *sign == b'-';
        rest = tail;
    }
    let mut power = 0i32;
    let mut power_digits = 0;
    while let [c @ b'0'..=b'9', tail @ ..] = rest {
        if power > 100_000 {
            return None;
        }
        power = power * 10 + (*c - b'0') as i32;
        power_digits += 1;
        rest = tail;
    }
    if let [b'L' | b'l'] = rest {
        rest = &[];
    }
    if power_digits == 0 || !rest.is_empty() {
        return None;
    }
    exponent += if exponent_negative { -power } else { power };
    let sign = if negative { 0x8000 } else { 0 };
    if mantissa == 0 {
        return Some(F80::from_parts(0, sign));
    }
    let top = 127 - mantissa.leading_zeros() as i32;
    // The value is in [2^scale, 2^(scale+1)).
    let scale = exponent + top;
    if scale > 16383 {
        return None;
    }
    // The weight of the significand's lowest bit, and the biased exponent.
    let (low, biased) = if scale >= -16382 {
        (scale - 63, (scale + 16383) as u16)
    } else {
        (-16382 - 63, 0)
    };
    let shift = low - exponent;
    let significand = if shift > 0 {
        if shift >= 128 || mantissa & ((1 << shift) - 1) != 0 {
            return None;
        }
        mantissa >> shift
    } else {
        // At most 63: the top bit lands at or below bit 63.
        mantissa << -shift
    };
    Some(F80::from_parts(significand as u64, sign | biased))
}

/// The `long double` a C hexadecimal constant names. Use it through
/// [`hexf80`], which evaluates it at compile time.
#[allow(
    clippy::panic,
    reason = "evaluated at compile time, where a panic fails the build"
)]
pub(crate) const fn hex80(text: &str) -> F80 {
    match parse_hex(text) {
        Some(value) => value,
        None => panic!("not exactly a long double"),
    }
}

/// A `long double` written as C writes it in hexadecimal, such as
/// `hexf80!("0x1p16383L")`, checked and evaluated at compile time.
macro_rules! hexf80 {
    ($text:literal) => {{
        const VALUE: $crate::math::ld80::F80 = $crate::math::ld80::hex80($text);
        VALUE
    }};
}
pub(crate) use hexf80;

/// Defines an exported `long double` function: the naked shim with the C
/// name, and the `extern "C"` adapter it calls, in a private module of the
/// same name, which calls the Rust function after `=`.
///
/// The shapes, with the registers each shim moves:
///
/// * `fn f(long double) -> long double`: `rdi` the argument, `rsi` the
///   result's slot.
/// * `fn f(long double, long double) -> long double`: `rdi` and `rsi` the
///   arguments, `rdx` the result's slot.
/// * `fn f(long double, n: T) -> long double`: `n` moves from `rdi` to
///   `rsi`, `rdi` the argument, `rdx` the result's slot.
/// * `fn f(p: T) -> long double`: `p` stays in `rdi`, `rsi` the result's
///   slot.
/// * `fn f(long double) -> T`: `rdi` the argument, and the adapter's return
///   is the function's.
/// * `fn f(x: T, long double) -> T`: `x` stays in its SSE register, `rdi`
///   the argument.
///
/// A shim returning a `long double` makes 24 bytes of room, which aligns the
/// stack for the call: the adapter writes the result to the bottom ten, and
/// the shim loads it. There the first `long double` argument is at
/// `rsp + 32` and the second at `rsp + 48`. A shim returning in a register
/// jumps to its adapter, with the argument at `rsp + 8`, so the adapter
/// returns straight to the caller. `@st0` writes the first kind of shim for
/// an adapter written by hand.
macro_rules! export {
    (@st0 $(#[$doc:meta])* fn $name:ident($($arg:ident: $ty:ty),*) via $abi:path;
        $($setup:literal),*) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// The caller must pass the arguments C declares, as C does. The Rust
        /// signature shows only those passed in registers, and no result: a
        /// `long double` is passed in the caller's stack frame and returned in
        /// `st(0)`, where no Rust type can name it.
        #[unsafe(naked)]
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name($($arg: $ty),*) {
            core::arch::naked_asm!(
                "sub rsp, 24",
                $($setup,)*
                "call {abi}",
                "fld tbyte ptr [rsp]",
                "add rsp, 24",
                "ret",
                abi = sym $abi,
            )
        }
    };
    (@register $(#[$doc:meta])* fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// The caller must pass the arguments C declares, as C does. The Rust
        /// signature does not show the `long double`, which is passed in the
        /// caller's stack frame, where no Rust type can name it.
        #[unsafe(naked)]
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
            core::arch::naked_asm!(
                "lea rdi, [rsp + 8]",
                "jmp {abi}",
                abi = sym $name::abi,
            )
        }
    };
    ($(#[$doc:meta])* fn $name:ident(long double) -> long double = $work:ident;) => {
        #[doc = concat!("The adapter `", stringify!($name), "`'s shim calls.")]
        mod $name {
            use super::*;

            #[doc = concat!("`", stringify!($work), "` of `*x`, stored in `*out`.")]
            pub(super) extern "C" fn abi(x: &F80, out: &mut F80) {
                *out = $work(*x);
            }
        }
        $crate::math::ld80::export!(@st0 $(#[$doc])* fn $name() via $name::abi;
            "lea rdi, [rsp + 32]", "mov rsi, rsp");
    };
    ($(#[$doc:meta])* fn $name:ident(long double, long double) -> long double = $work:ident;) => {
        #[doc = concat!("The adapter `", stringify!($name), "`'s shim calls.")]
        mod $name {
            use super::*;

            #[doc = concat!("`", stringify!($work), "` of `*x` and `*y`, stored in `*out`.")]
            pub(super) extern "C" fn abi(x: &F80, y: &F80, out: &mut F80) {
                *out = $work(*x, *y);
            }
        }
        $crate::math::ld80::export!(@st0 $(#[$doc])* fn $name() via $name::abi;
            "lea rdi, [rsp + 32]", "lea rsi, [rsp + 48]", "mov rdx, rsp");
    };
    ($(#[$doc:meta])* fn $name:ident(long double, long double, $arg:ident: $ty:ty) -> long double = $work:ident;) => {
        #[doc = concat!("The adapter `", stringify!($name), "`'s shim calls.")]
        mod $name {
            use super::*;

            #[doc = concat!("`", stringify!($work), "` of `*x`, `*y` and the pointer, stored in `*out`.")]
            pub(super) extern "C" fn abi(x: &F80, y: &F80, $arg: $ty, out: &mut F80) {
                *out = $work(*x, *y, $arg);
            }
        }
        $crate::math::ld80::export!(@st0 $(#[$doc])* fn $name($arg: $ty) via $name::abi;
            "mov rdx, rdi", "lea rdi, [rsp + 32]", "lea rsi, [rsp + 48]", "mov rcx, rsp");
    };
    ($(#[$doc:meta])* fn $name:ident(long double, $arg:ident: $ty:ty) -> long double = $work:ident;) => {
        #[doc = concat!("The adapter `", stringify!($name), "`'s shim calls.")]
        mod $name {
            use super::*;

            #[doc = concat!("`", stringify!($work), "` of `*x` and the integer, stored in `*out`.")]
            pub(super) extern "C" fn abi(x: &F80, $arg: $ty, out: &mut F80) {
                *out = $work(*x, $arg);
            }
        }
        $crate::math::ld80::export!(@st0 $(#[$doc])* fn $name($arg: $ty) via $name::abi;
            "mov rsi, rdi", "lea rdi, [rsp + 32]", "mov rdx, rsp");
    };
    ($(#[$doc:meta])* fn $name:ident($arg:ident: $ty:ty) -> long double = $work:ident;) => {
        #[doc = concat!("The adapter `", stringify!($name), "`'s shim calls.")]
        mod $name {
            use super::*;

            #[doc = concat!("`", stringify!($work), "` of the argument, stored in `*out`.")]
            pub(super) extern "C" fn abi($arg: $ty, out: &mut F80) {
                *out = $work($arg);
            }
        }
        $crate::math::ld80::export!(@st0 $(#[$doc])* fn $name($arg: $ty) via $name::abi;
            "mov rsi, rsp");
    };
    ($(#[$doc:meta])* fn $name:ident(long double) -> $ret:ty = $work:ident;) => {
        #[doc = concat!("The adapter `", stringify!($name), "`'s shim jumps to.")]
        mod $name {
            use super::*;

            #[doc = concat!("`", stringify!($work), "` of `*x`.")]
            pub(super) extern "C" fn abi(x: &F80) -> $ret {
                $work(*x)
            }
        }
        $crate::math::ld80::export!(@register $(#[$doc])* fn $name() -> $ret);
    };
    ($(#[$doc:meta])* fn $name:ident($arg:ident: $ty:ty, long double) -> $ret:ty = $work:ident;) => {
        #[doc = concat!("The adapter `", stringify!($name), "`'s shim jumps to.")]
        mod $name {
            use super::*;

            #[doc = concat!("`", stringify!($work), "` of the argument and `*y`.")]
            pub(super) extern "C" fn abi($arg: $ty, y: &F80) -> $ret {
                $work($arg, *y)
            }
        }
        $crate::math::ld80::export!(@register $(#[$doc])* fn $name($arg: $ty) -> $ret);
    };
}
pub(crate) use export;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::fenv::{FE_DIVBYZERO, FE_INEXACT, FE_INVALID, FE_TONEAREST, FE_UPWARD};
    use crate::math::mtest;

    /// The bits of a `long double`, as significand and sign-and-exponent.
    fn parts(x: F80) -> (u64, u16) {
        (x.mantissa(), x.sign_exponent())
    }

    #[test]
    fn hexadecimal_constants_parse_exactly() {
        let parse = |text| parse_hex(text).map(parts);
        assert_eq!(parse("0x1p+0L"), Some((1 << 63, 0x3fff)));
        assert_eq!(parse("-0x0p+0"), Some((0, 0x8000)));
        assert_eq!(
            parse("-0x1.02239f3c6a8f13dep+3L"),
            Some((0x8111_cf9e_3547_89ef, 0xc002))
        );
        assert_eq!(
            parse("0x1.fffffffffffffffep+16383L"),
            Some((u64::MAX, 0x7ffe))
        );
        assert_eq!(parse("0x1p-16382L"), Some((1 << 63, 1)));
        assert_eq!(parse("0x1p-16445L"), Some((1, 0)));
        assert_eq!(parse("0x0.8p-16444"), Some((1, 0)));
        assert_eq!(parse("0x1.8p-16444"), Some((3, 0)));
        assert_eq!(parse("0x1p-16446L"), None);
        assert_eq!(parse("0x1p+16384L"), None);
        assert_eq!(parse("0x1.00000000000000008p0"), None);
        assert_eq!(parse("0x1p0x"), None);
        assert_eq!(parts(hexf80!("0x1p16383L")), (1 << 63, 0x7ffe));
    }

    #[test]
    fn arithmetic_rounds_in_the_current_mode_and_raises() {
        let one = F80::ONE;
        let tiny = F80::from_parts(1 << 63, 0x3fff - 64);
        let (sum, raised) = mtest::under(FE_TONEAREST, || one.add(tiny));
        assert_eq!(parts(sum), (1 << 63, 0x3fff));
        assert_eq!(raised, FE_INEXACT);
        let (sum, raised) = mtest::under(FE_UPWARD, || one.add(tiny));
        assert_eq!(parts(sum), ((1 << 63) + 1, 0x3fff));
        assert_eq!(raised, FE_INEXACT);
        let (quotient, raised) = mtest::under(FE_TONEAREST, || one.div(F80::ZERO));
        assert_eq!(parts(quotient), (1 << 63, 0x7fff));
        assert_eq!(raised, FE_DIVBYZERO);
        let three = F80::from_parts(0xc000_0000_0000_0000, 0x4000);
        assert_eq!(parts(three.sub(one)), (1 << 63, 0x4000));
        assert_eq!(parts(three.mul(three)), (0x9000_0000_0000_0000, 0x4002));
        let (root, raised) = mtest::under(FE_TONEAREST, || one.neg().sqrt());
        assert_eq!(parts(root), (0xc000_0000_0000_0000, 0xffff));
        assert_eq!(raised, FE_INVALID);
    }

    #[test]
    fn comparisons_read_the_flags() {
        let one = F80::ONE;
        let two = F80::from_parts(1 << 63, 0x4000);
        assert!(F80::fcomi(two, one).above());
        assert!(F80::fcomi(one, two).below());
        assert!(F80::fucomi(one, one).equal());
        assert!(F80::fucomi(F80::ZERO, F80::ZERO.neg()).equal());
        let (flags, raised) = mtest::under(FE_TONEAREST, || F80::fucomi(F80::NAN, one));
        assert!(!flags.equal() && flags.below() && !flags.above());
        assert_eq!(raised, 0);
        let (_, raised) = mtest::under(FE_TONEAREST, || F80::fcomi(F80::NAN, one));
        assert_eq!(raised, FE_INVALID);
    }

    #[test]
    fn conversions_and_remainders() {
        let seven = F80::from_i32(7);
        assert_eq!(parts(seven), (0xe000_0000_0000_0000, 0x4001));
        assert_eq!(seven.to_f64(), 7.0);
        assert_eq!(parts(F80::from_f32(-0.5)), (1 << 63, 0xbffe));
        let two = F80::from_f64(2.0);
        assert_eq!(parts(seven.fprem(two)), (1 << 63, 0x3fff));
        let (remainder, status) = seven.fprem1(two);
        assert_eq!(parts(remainder), (1 << 63, 0xbfff));
        // A quotient of 4: C0, C3, C1 hold its bits 2, 1 and 0.
        assert_eq!(status & 0x4700, 0x0100);
        let half = F80::from_f64(2.5);
        assert_eq!(parts(half.rndint()), (1 << 63, 0x4000));
        assert_eq!(
            parts(half.rndint_with_control(0x0b)),
            (0xc000_0000_0000_0000, 0x4000)
        );
        assert_eq!(half.neg().to_i64(), -2);
        assert_eq!(half.neg().to_i64_truncating(), -2);
        assert_eq!(F80::from_f64(-2.75).to_i64_truncating(), -2);
        assert_eq!(parts(seven.add_f64(0.5)), (0xf000_0000_0000_0000, 0x4001));
        assert_eq!(parts(seven.add_f32(1.0)), (1 << 63, 0x4002));
        assert_eq!(seven.to_f32(), 7.0);
        assert_eq!(parts(seven.neg().abs()), parts(seven));
    }
}
