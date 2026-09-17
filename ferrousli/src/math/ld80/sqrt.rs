//! `sqrtl`, the `long double` square root.
//!
//! Ported from musl 1.2.5's `x86_64/sqrtl.c` (MIT; see [`crate::math`] for the
//! notice), which is `fsqrt` and nothing else: the x87 rounds it correctly in
//! the current mode, raising inexact when it rounds and invalid for a negative
//! argument.

use crate::math::ld80::{F80, export};

/// The square root of `x`, correctly rounded.
fn sqrt_work(x: F80) -> F80 {
    x.sqrt()
}

export! {
    /// `sqrtl`: the square root of a `long double`.
    fn sqrtl(long double) -> long double = sqrt_work;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fenv::{FE_INEXACT, FE_INVALID, FE_TONEAREST};
    use crate::math::ld80::hexf80;
    use crate::math::mtest;

    /// The bits of a `long double`, as significand and sign-and-exponent.
    fn parts(x: F80) -> (u64, u16) {
        (x.mantissa(), x.sign_exponent())
    }

    #[test]
    fn the_square_root_is_exact_where_it_can_be() {
        assert_eq!(
            parts(sqrt_work(hexf80!("0x1p+8L"))),
            parts(F80::from_i32(16))
        );
        let (root, raised) = mtest::under(FE_TONEAREST, || sqrt_work(F80::from_i32(2)));
        assert_eq!(raised, FE_INEXACT);
        assert_eq!(root.to_f64(), core::f64::consts::SQRT_2);
        let (root, raised) = mtest::under(FE_TONEAREST, || sqrt_work(F80::ONE.neg()));
        assert!(root.is_nan());
        assert_eq!(raised, FE_INVALID);
        // A zero keeps its sign and raises nothing.
        let (root, raised) = mtest::under(FE_TONEAREST, || sqrt_work(F80::ZERO.neg()));
        assert_eq!(parts(root), parts(F80::ZERO.neg()));
        assert_eq!(raised, 0);
    }
}
