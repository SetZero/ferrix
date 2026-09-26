//! `logl`, as a stand-in: `log` of the argument rounded to `double`, the
//! result widened back, as [`super::pow`]'s `powl` is.
//!
//! musl 1.2.5 ports Cephes' `logl` for the x87 format; that port is not done
//! yet. LLVM, which `rustc` loads, imports `logl`, and the loader binds every
//! import at load, so without a definition the compiler does not start. Its
//! results carry `double`'s 53 bits, not the format's 64.

use crate::math::ld80::{F80, export};
use crate::math::log::log;

/// The natural logarithm of `x`, in `double`.
fn log_work(x: F80) -> F80 {
    F80::from_f64(log(x.to_f64()))
}

export! {
    /// `logl`: the natural logarithm of `x`, computed in `double`.
    fn logl(long double) -> long double = log_work;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logl_is_log_widened() {
        for (x, want) in [
            (1.0, 0.0),
            (core::f64::consts::E, 1.0),
            (0.0, f64::NEG_INFINITY),
        ] {
            assert_eq!(log_work(F80::from_f64(x)).to_f64(), want, "logl({x})");
        }
        assert!(log_work(F80::from_f64(-1.0)).to_f64().is_nan());
    }
}
