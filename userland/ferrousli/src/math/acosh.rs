//! `acosh` and `acoshf`: the inverse hyperbolic cosine.
//!
//! Ported from musl 1.2.5's `acosh.c` and `acoshf.c` (MIT; see
//! [`crate::math`] for the notice). musl wrote these itself; they carry no
//! other notice.
//!
//! # Method
//!
//! acosh(x) = log(x + √(x² - 1)): below 2 as log1p((x - 1) + √((x - 1)² +
//! 2(x - 1))), below 2^26 (2^12 for `float`) as log(2x - 1/(x + √(x² - 1))),
//! and above as log(x) + ln2. For x < 1 the logarithm or the root raises
//! invalid.

use crate::math::asinh::{LN2, LN2_F};
use crate::math::log::log;
use crate::math::log1p::{log1p, log1pf};
use crate::math::logf::logf;
use crate::math::sqrt::{sqrt, sqrtf};
use crate::math::support::{barrier, barrierf};

/// The inverse hyperbolic cosine of `x`, for `x` >= 1.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn acosh(x: f64) -> f64 {
    let e = (x.to_bits() >> 52 & 0x7ff) as u32;

    if e < 0x3ff + 1 {
        // |x| < 2, up to 2 ulps of error in [1, 1.125].
        return log1p(x - 1.0 + sqrt((x - 1.0) * (x - 1.0) + 2.0 * (x - 1.0)));
    }
    if e < 0x3ff + 26 {
        // |x| < 2^26. LLVM would rewrite `a - 1/b` as `a + -1/b`, which
        // rounds the quotient the other way when rounding up or down.
        return log(2.0 * x - barrier(1.0 / (x + sqrt(x * x - 1.0))));
    }
    // |x| >= 2^26, or NaN.
    log(x) + LN2
}

/// [`acosh`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn acoshf(x: f32) -> f32 {
    let bits = x.to_bits();
    let a = bits & 0x7fff_ffff;

    if a < 0x3f80_0000 + (1 << 23) {
        // |x| < 2, invalid if x < 1; up to 2 ulps of error in [1, 1.125].
        return log1pf(x - 1.0 + sqrtf((x - 1.0) * (x - 1.0) + 2.0 * (x - 1.0)));
    }
    if bits < 0x3f80_0000 + (12 << 23) {
        // 2 <= x < 2^12. As in `acosh`.
        return logf(2.0 * x - barrierf(1.0 / (x + sqrtf(x * x - 1.0))));
    }
    // x >= 2^12, or x <= -2, or NaN.
    logf(x) + LN2_F
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn acosh_matches_libc_test() {
        let files = ["sanity/acosh.h", "special/acosh.h"];
        // libc-test's `acosh.c` tolerates an error under 2 ulps.
        let rules = Rules::ULP.tolerate(2.0);
        mtest::d_d("acosh", &files, |x| acosh(x), rules, &[]);
    }

    #[test]
    fn acoshf_matches_libc_test() {
        let files = ["sanity/acoshf.h", "special/acoshf.h"];
        mtest::d_d("acoshf", &files, |x| acoshf(x), Rules::ULP, &[]);
    }
}
