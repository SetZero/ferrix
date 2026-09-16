//! `logf` and `log2f`, and the table of base-2 logarithms that `powf` shares.
//!
//! Ported from musl 1.2.5's `logf.c`, `logf_data.c`, `logf_data.h`,
//! `log2f.c`, `log2f_data.c` and `log2f_data.h` (MIT; see [`crate::math`] for
//! the notice). musl took them from ARM's optimized-routines, whose files
//! carry this notice:
//!
//! ```text
//! Copyright (c) 2017-2018, Arm Limited.
//! SPDX-License-Identifier: MIT
//! ```
//!
//! The tables were converted from `logf_data.c` and `log2f_data.c` by a
//! script, digit for digit. The arithmetic is in `double`, and only the
//! result is rounded to `float`.

use crate::math::support::{divzerof, hexf32, hexf64, invalidf};

/// `LOGF_TABLE_BITS` and `LOG2F_TABLE_BITS`: z's range is split into
/// N = 2^4 subintervals.
pub(crate) const TABLE_BITS: u32 = 4;
/// `N`.
pub(crate) const N: u32 = 1 << TABLE_BITS;
/// The bottom of z's range, [`OFF`, 2·`OFF`].
pub(crate) const OFF: u32 = 0x3f33_0000;

/// ln2.
const LN2: f64 = hexf64!("0x1.62e42fefa39efp-1");

// `logf`'s `poly`: log1p(r)'s coefficients of r⁴, r³ and r². The coefficient
// of r is 1.
const A0: f64 = hexf64!("-0x1.00ea348b88334p-2");
const A1: f64 = hexf64!("0x1.5575b0be00b6ap-2");
const A2: f64 = hexf64!("-0x1.ffffef20a4123p-2");

// `log2f`'s `poly`: log1p(r)/ln2's coefficients of r⁴, r³, r² and r.
const B0: f64 = hexf64!("-0x1.712b6f70a7e4dp-2");
const B1: f64 = hexf64!("0x1.ecabf496832ep-2");
const B2: f64 = hexf64!("-0x1.715479ffae3dep-1");
const B3: f64 = hexf64!("0x1.715475f35c8b8p0");

/// A table row of two `double`s, written as musl's table writes it.
macro_rules! pair {
    ($a:literal, $b:literal) => {
        (hexf64!($a), hexf64!($b))
    };
}

/// `logf`'s `tab`: 1/c and log(c) for the c near the middle of each
/// subinterval.
static TABLE: [(f64, f64); N as usize] = [
    pair!("0x1.661ec79f8f3bep+0", "-0x1.57bf7808caadep-2"),
    pair!("0x1.571ed4aaf883dp+0", "-0x1.2bef0a7c06ddbp-2"),
    pair!("0x1.49539f0f010bp+0", "-0x1.01eae7f513a67p-2"),
    pair!("0x1.3c995b0b80385p+0", "-0x1.b31d8a68224e9p-3"),
    pair!("0x1.30d190c8864a5p+0", "-0x1.6574f0ac07758p-3"),
    pair!("0x1.25e227b0b8eap+0", "-0x1.1aa2bc79c81p-3"),
    pair!("0x1.1bb4a4a1a343fp+0", "-0x1.a4e76ce8c0e5ep-4"),
    pair!("0x1.12358f08ae5bap+0", "-0x1.1973c5a611cccp-4"),
    pair!("0x1.0953f419900a7p+0", "-0x1.252f438e10c1ep-5"),
    pair!("0x1p+0", "0x0p+0"),
    pair!("0x1.e608cfd9a47acp-1", "0x1.aa5aa5df25984p-5"),
    pair!("0x1.ca4b31f026aap-1", "0x1.c5e53aa362eb4p-4"),
    pair!("0x1.b2036576afce6p-1", "0x1.526e57720db08p-3"),
    pair!("0x1.9c2d163a1aa2dp-1", "0x1.bc2860d22477p-3"),
    pair!("0x1.886e6037841edp-1", "0x1.1058bc8a07ee1p-2"),
    pair!("0x1.767dcf5534862p-1", "0x1.4043057b6ee09p-2"),
];

/// `log2f`'s `tab`: 1/c, the same as [`TABLE`]'s, and log2(c).
static TABLE2: [(f64, f64); N as usize] = [
    pair!("0x1.661ec79f8f3bep+0", "-0x1.efec65b963019p-2"),
    pair!("0x1.571ed4aaf883dp+0", "-0x1.b0b6832d4fca4p-2"),
    pair!("0x1.49539f0f010bp+0", "-0x1.7418b0a1fb77bp-2"),
    pair!("0x1.3c995b0b80385p+0", "-0x1.39de91a6dcf7bp-2"),
    pair!("0x1.30d190c8864a5p+0", "-0x1.01d9bf3f2b631p-2"),
    pair!("0x1.25e227b0b8eap+0", "-0x1.97c1d1b3b7afp-3"),
    pair!("0x1.1bb4a4a1a343fp+0", "-0x1.2f9e393af3c9fp-3"),
    pair!("0x1.12358f08ae5bap+0", "-0x1.960cbbf788d5cp-4"),
    pair!("0x1.0953f419900a7p+0", "-0x1.a6f9db6475fcep-5"),
    pair!("0x1p+0", "0x0p+0"),
    pair!("0x1.e608cfd9a47acp-1", "0x1.338ca9f24f53dp-4"),
    pair!("0x1.ca4b31f026aap-1", "0x1.476a9543891bap-3"),
    pair!("0x1.b2036576afce6p-1", "0x1.e840b4ac4e4d2p-3"),
    pair!("0x1.9c2d163a1aa2dp-1", "0x1.40645f0c6651cp-2"),
    pair!("0x1.886e6037841edp-1", "0x1.88e9c2c1b9ff8p-2"),
    pair!("0x1.767dcf5534862p-1", "0x1.ce0a44eb17bccp-2"),
];

/// Row `i` of `table`, or a row for c = 1 if there is none.
fn row(table: &[(f64, f64)], i: u32) -> (f64, f64) {
    usize::try_from(i)
        .ok()
        .and_then(|i| table.get(i))
        .copied()
        .unwrap_or((1.0, 0.0))
}

/// Row `i` of `log2f`'s table, which `powf`'s is: its `POWF_SCALE` is 1.
pub(crate) fn log2_row(i: u32) -> (f64, f64) {
    row(&TABLE2, i)
}

/// The natural logarithm of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn logf(x: f32) -> f32 {
    let mut ix = x.to_bits();
    // Get the sign of zero right when rounding downward with x == 1.
    if ix == 0x3f80_0000 {
        return 0.0;
    }
    if ix.wrapping_sub(0x0080_0000) >= 0x7f80_0000 - 0x0080_0000 {
        // x < 2^-126, or infinite, or NaN.
        if ix.wrapping_mul(2) == 0 {
            return divzerof(1);
        }
        if ix == 0x7f80_0000 {
            return x;
        }
        if ix & 0x8000_0000 != 0 || ix.wrapping_mul(2) >= 0xff00_0000 {
            return invalidf(x);
        }
        // x is subnormal: normalise it.
        ix = (x * hexf32!("0x1p23")).to_bits();
        ix = ix.wrapping_sub(23 << 23);
    }

    // x = 2^k·z, where z is in [OFF, 2·OFF] and exact. z's range is split into
    // N subintervals, and c is near the middle of z's.
    let tmp = ix.wrapping_sub(OFF);
    let i = (tmp >> (23 - TABLE_BITS)) % N;
    // An arithmetic shift.
    let k = tmp.cast_signed() >> 23;
    let iz = ix.wrapping_sub(tmp & 0xff80_0000);
    let (invc, logc) = row(&TABLE, i);
    let z = f64::from(f32::from_bits(iz));

    // log(x) = log1p(z/c - 1) + log(c) + k·ln2.
    let r = z * invc - 1.0;
    let y0 = logc + f64::from(k) * LN2;

    // log1p(r), evaluated in pieces for a pipelined processor.
    let r2 = r * r;
    let mut y = A1 * r + A2;
    y += A0 * r2;
    y = y * r2 + (y0 + r);
    y as f32
}

/// The base-2 logarithm of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn log2f(x: f32) -> f32 {
    let mut ix = x.to_bits();
    // Get the sign of zero right when rounding downward with x == 1.
    if ix == 0x3f80_0000 {
        return 0.0;
    }
    if ix.wrapping_sub(0x0080_0000) >= 0x7f80_0000 - 0x0080_0000 {
        // x < 2^-126, or infinite, or NaN.
        if ix.wrapping_mul(2) == 0 {
            return divzerof(1);
        }
        if ix == 0x7f80_0000 {
            return x;
        }
        if ix & 0x8000_0000 != 0 || ix.wrapping_mul(2) >= 0xff00_0000 {
            return invalidf(x);
        }
        // x is subnormal: normalise it.
        ix = (x * hexf32!("0x1p23")).to_bits();
        ix = ix.wrapping_sub(23 << 23);
    }

    // x = 2^k·z, where z is in [OFF, 2·OFF] and exact. z's range is split into
    // N subintervals, and c is near the middle of z's.
    let tmp = ix.wrapping_sub(OFF);
    let i = (tmp >> (23 - TABLE_BITS)) % N;
    let top = tmp & 0xff80_0000;
    let iz = ix.wrapping_sub(top);
    // An arithmetic shift.
    let k = tmp.cast_signed() >> 23;
    let (invc, logc) = row(&TABLE2, i);
    let z = f64::from(f32::from_bits(iz));

    // log2(x) = log1p(z/c - 1)/ln2 + log2(c) + k.
    let r = z * invc - 1.0;
    let y0 = logc + f64::from(k);

    // log1p(r)/ln2, evaluated in pieces for a pipelined processor.
    let r2 = r * r;
    let mut y = B1 * r + B2;
    y += B0 * r2;
    let p = B3 * r + y0;
    y = y * r2 + p;
    y as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn logf_matches_libc_test() {
        let files = ["ucb/logf.h", "sanity/logf.h", "special/logf.h"];
        mtest::d_d("logf", &files, |x| logf(x), Rules::ULP, &[]);
    }

    #[test]
    fn log2f_matches_libc_test() {
        let files = ["sanity/log2f.h", "special/log2f.h"];
        mtest::d_d("log2f", &files, |x| log2f(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_tables_start_and_end_as_musls() {
        assert_eq!(
            TABLE.first(),
            Some(&pair!("0x1.661ec79f8f3bep+0", "-0x1.57bf7808caadep-2"))
        );
        assert_eq!(
            TABLE.last(),
            Some(&pair!("0x1.767dcf5534862p-1", "0x1.4043057b6ee09p-2"))
        );
        assert_eq!(
            TABLE2.first(),
            Some(&pair!("0x1.661ec79f8f3bep+0", "-0x1.efec65b963019p-2"))
        );
        assert_eq!(
            log2_row(N - 1),
            pair!("0x1.767dcf5534862p-1", "0x1.ce0a44eb17bccp-2")
        );
        for i in 0..N {
            assert_eq!(row(&TABLE, i).0, log2_row(i).0);
        }
    }
}
