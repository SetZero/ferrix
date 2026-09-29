//! `stdlib.h` and `inttypes.h`: integer absolute values and division.
//!
//! C leaves `abs(INT_MIN)`, division by zero and `INT_MIN / -1` undefined.
//! Here the absolute value and the overflowing quotient wrap, as the hardware
//! would give them, and division by zero traps rather than returning a value
//! the program never asked for.

use core::ffi::{c_int, c_long, c_longlong};

use crate::syscall;

/// `intmax_t`: 64 bits on every target, a `long` on the 64-bit ones and a
/// `long long` on ARMv7-A, as `bits/alltypes.h` defines it.
type IntMax = i64;

/// `div_t`, `{ int quot, rem; }`. Eight bytes, which the SysV ABI returns in
/// `rax`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DivT {
    /// The quotient, truncated toward zero.
    pub quot: c_int,
    /// The remainder, with the dividend's sign.
    pub rem: c_int,
}

/// `ldiv_t`, `{ long quot, rem; }`, returned in `rax` and `rdx`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LdivT {
    /// The quotient, truncated toward zero.
    pub quot: c_long,
    /// The remainder, with the dividend's sign.
    pub rem: c_long,
}

/// `lldiv_t`, `{ long long quot, rem; }`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LldivT {
    /// The quotient, truncated toward zero.
    pub quot: c_longlong,
    /// The remainder, with the dividend's sign.
    pub rem: c_longlong,
}

/// `imaxdiv_t`, `{ intmax_t quot, rem; }` in `inttypes.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImaxdivT {
    /// The quotient, truncated toward zero.
    pub quot: IntMax,
    /// The remainder, with the dividend's sign.
    pub rem: IntMax,
}

const _: () = assert!(size_of::<DivT>() == 8 && align_of::<DivT>() == 4);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<LdivT>() == 16 && core::mem::offset_of!(LdivT, rem) == 8);
const _: () = assert!(size_of::<LldivT>() == 16 && core::mem::offset_of!(LldivT, rem) == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<ImaxdivT>() == 16 && core::mem::offset_of!(ImaxdivT, rem) == 8);

/// Defines an absolute value function.
macro_rules! abs_fn {
    ($(#[$doc:meta])* $name:ident, $ty:ty) => {
        $(#[$doc])*
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub extern "C" fn $name(n: $ty) -> $ty {
            n.wrapping_abs()
        }
    };
}

abs_fn!(
    /// The absolute value of an `int`.
    abs, c_int
);
abs_fn!(
    /// The absolute value of a `long`.
    labs, c_long
);
abs_fn!(
    /// The absolute value of a `long long`.
    llabs, c_longlong
);
abs_fn!(
    /// The absolute value of an `intmax_t`.
    imaxabs, IntMax
);

/// Defines a division function returning quotient and remainder.
macro_rules! div_fn {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $out:ident) => {
        $(#[$doc])*
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub extern "C" fn $name(num: $ty, den: $ty) -> $out {
            if den == 0 {
                syscall::trap();
            }
            $out {
                quot: num.wrapping_div(den),
                rem: num.wrapping_rem(den),
            }
        }
    };
}

div_fn!(
    /// Divides two `int`s, truncating toward zero.
    div, c_int, DivT
);
div_fn!(
    /// Divides two `long`s, truncating toward zero.
    ldiv, c_long, LdivT
);
div_fn!(
    /// Divides two `long long`s, truncating toward zero.
    lldiv, c_longlong, LldivT
);
div_fn!(
    /// Divides two `intmax_t`s, truncating toward zero.
    imaxdiv, IntMax, ImaxdivT
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_values_wrap_at_the_minimum() {
        assert_eq!(abs(-5), 5);
        assert_eq!(abs(c_int::MIN), c_int::MIN);
        assert_eq!(labs(-7), 7);
        assert_eq!(llabs(c_longlong::MIN + 1), c_longlong::MAX);
        assert_eq!(imaxabs(3), 3);
    }

    #[test]
    fn division_truncates_toward_zero() {
        assert_eq!(div(7, -2), DivT { quot: -3, rem: 1 });
        assert_eq!(div(-7, 2), DivT { quot: -3, rem: -1 });
        assert_eq!(
            div(c_int::MIN, -1),
            DivT {
                quot: c_int::MIN,
                rem: 0
            }
        );
        assert_eq!(ldiv(-9, 4), LdivT { quot: -2, rem: -1 });
        assert_eq!(
            lldiv(1 << 40, 3),
            LldivT {
                quot: 366_503_875_925,
                rem: 1
            }
        );
        assert_eq!(imaxdiv(-1, 5), ImaxdivT { quot: 0, rem: -1 });
    }
}
