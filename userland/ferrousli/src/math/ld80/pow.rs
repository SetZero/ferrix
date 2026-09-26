//! `powl`, as a stand-in: `pow` of the arguments rounded to `double`, the
//! result widened back.
//!
//! musl 1.2.5 has a full `long double` `powl` for the x87 format, ported
//! from Cephes; that port is not done yet. Chrome imports `powl`, and the
//! loader binds every import at load, so without a definition the program
//! does not start at all. This one is what musl itself does for AArch64's
//! binary128 `long double`: the `double` function. Its results carry
//! `double`'s 53 bits, not the format's 64, and an argument outside
//! `double`'s range is rounded to its infinity or zero first.

use crate::math::ld80::{F80, export};
use crate::math::pow::pow;

/// `x` to the power `y`, in `double`.
fn pow_work(x: F80, y: F80) -> F80 {
    F80::from_f64(pow(x.to_f64(), y.to_f64()))
}

export! {
    /// `powl`: `x` to the power `y`, computed in `double`.
    fn powl(long double, long double) -> long double = pow_work;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powl_is_pow_widened() {
        let cases = [
            (2.0, 10.0, 1024.0),
            (9.0, 0.5, 3.0),
            (-2.0, 3.0, -8.0),
            (0.0, -1.0, f64::INFINITY),
        ];
        for (x, y, want) in cases {
            let got = pow_work(F80::from_f64(x), F80::from_f64(y)).to_f64();
            assert_eq!(got, want, "powl({x}, {y})");
        }
        assert!(
            pow_work(F80::from_f64(-1.0), F80::from_f64(0.5))
                .to_f64()
                .is_nan()
        );
    }
}
