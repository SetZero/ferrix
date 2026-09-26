//! `tgamma` and `tgammaf`: the gamma function.
//!
//! Ported from musl 1.2.5's `tgamma.c` and `tgammaf.c` (MIT; see
//! [`crate::math`] for the notice). musl wrote these itself, with ideas and
//! constants from Boost and Python; they carry no other notice.
//!
//! musl writes these constants in decimal. They are written here as bits, and
//! the tests check them against musl's decimals.
//!
//! # Method
//!
//! Lanczos' approximation, with g = 6.0246800407767295837...:
//! Γ(x) = (x + g - 1/2)^(x - 1/2) · S(x) / exp(x + g - 1/2), where S is a
//! rational function of degree 12, and the error of rounding x + g - 1/2 is
//! corrected for. A negative x reflects: Γ(x) = -π/(x·sin(πx)·Γ(-x)). Positive
//! integers up to 23 come from a table of factorials. `tgammaf` rounds
//! `tgamma`'s result.

use crate::math::arch::trunc_to_i64;
use crate::math::exp::exp;
use crate::math::pow::pow;
use crate::math::rounding::floor;
use crate::math::support::{barrier, force_evalf, hexf64};
use crate::math::trig::{kernel_cos, kernel_sin};

/// π, rounded.
const PI: f64 = f64::from_bits(0x4009_21fb_5444_2d18);

/// g - 1/2, which is exact.
const GMHALF: f64 = f64::from_bits(0x4016_1945_b980_0000);

// S(x)'s numerator and denominator, from the constant term up.
const SNUM: [f64; 13] = [
    f64::from_bits(0x4215_ea51_43c1_a49e),
    f64::from_bits(0x4223_fc70_75f5_4c57),
    f64::from_bits(0x4220_a132_818a_b61a),
    f64::from_bits(0x4210_b0b5_22e8_261a),
    f64::from_bits(0x41f6_7fc1_b3a5_a1e8),
    f64::from_bits(0x41d5_7418_f5d3_f33f),
    f64::from_bits(0x41ad_ab0c_7bb9_5f2a),
    f64::from_bits(0x417d_f876_f95d_cc98),
    f64::from_bits(0x4145_f1e9_5080_f44c),
    f64::from_bits(0x4106_b642_1f87_87eb),
    f64::from_bits(0x40bf_87ac_0858_d804),
    f64::from_bits(0x406a_5a60_7bbc_3b52),
    f64::from_bits(0x4004_0d93_1ff6_2705),
];
const SDEN: [f64; 13] = [
    f64::from_bits(0x0000_0000_0000_0000),
    f64::from_bits(0x4183_08a8_0000_0000),
    f64::from_bits(0x419c_bd69_8000_0000),
    f64::from_bits(0x41a1_fda6_b000_0000),
    f64::from_bits(0x4199_1871_7000_0000),
    f64::from_bits(0x4185_eeb6_9000_0000),
    f64::from_bits(0x4169_7171_e000_0000),
    f64::from_bits(0x4144_1f7b_0000_0000),
    f64::from_bits(0x4115_d0bc_0000_0000),
    f64::from_bits(0x40df_e780_0000_0000),
    f64::from_bits(0x409e_1400_0000_0000),
    f64::from_bits(0x4050_8000_0000_0000),
    f64::from_bits(0x3ff0_0000_0000_0000),
];

/// n! for n from 0 to 22.
const FACT: [f64; 23] = [
    f64::from_bits(0x3ff0_0000_0000_0000),
    f64::from_bits(0x3ff0_0000_0000_0000),
    f64::from_bits(0x4000_0000_0000_0000),
    f64::from_bits(0x4018_0000_0000_0000),
    f64::from_bits(0x4038_0000_0000_0000),
    f64::from_bits(0x405e_0000_0000_0000),
    f64::from_bits(0x4086_8000_0000_0000),
    f64::from_bits(0x40b3_b000_0000_0000),
    f64::from_bits(0x40e3_b000_0000_0000),
    f64::from_bits(0x4116_2600_0000_0000),
    f64::from_bits(0x414b_af80_0000_0000),
    f64::from_bits(0x4183_08a8_0000_0000),
    f64::from_bits(0x41bc_8cfc_0000_0000),
    f64::from_bits(0x41f7_328c_c000_0000),
    f64::from_bits(0x4234_4c3b_2800_0000),
    f64::from_bits(0x4273_0777_7580_0000),
    f64::from_bits(0x42b3_0777_7580_0000),
    f64::from_bits(0x42f4_37ee_ecd8_0000),
    f64::from_bits(0x4336_beec_ca73_0000),
    f64::from_bits(0x437b_02b9_3068_9000),
    f64::from_bits(0x43c0_e1b3_be41_5a00),
    f64::from_bits(0x4406_283b_e9b5_c620),
    f64::from_bits(0x444e_7752_6159_f06c),
];

/// sin(π`x`) for `x` > 2^-100. Where it is 0, its sign is arbitrary. musl's
/// `sinpi`.
fn sinpi(x: f64) -> f64 {
    // x = |x| mod 2, with a spurious inexact for an odd integer.
    let x = x * 0.5;
    let x = 2.0 * (x - floor(x));

    // Reduce x to [-1/4, 1/4]. The conversion raises inexact as C's does.
    let n = trunc_to_i64(4.0 * x) as i32;
    let n = (n + 1) / 2;
    let x = x - f64::from(n) * 0.5;

    let x = x * PI;
    match n {
        1 => kernel_cos(x, 0.0),
        2 => kernel_sin(-x, 0.0, false),
        3 => -kernel_cos(x, 0.0),
        _ => kernel_sin(x, 0.0, false),
    }
}

/// S(`x`), the rational function, for positive `x`. musl's `S`.
fn lanczos_sum(x: f64) -> f64 {
    let mut num = 0.0;
    let mut den = 0.0;
    if x < 8.0 {
        for (n, d) in SNUM.iter().zip(&SDEN).rev() {
            num = num * x + n;
            den = den * x + d;
        }
    } else {
        // In 1/x, to avoid overflow.
        for (n, d) in SNUM.iter().zip(&SDEN) {
            num = num / x + n;
            den = den / x + d;
        }
    }
    num / den
}

/// Γ(`x`), the gamma function.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tgamma(x: f64) -> f64 {
    let u = x.to_bits();
    let ix = (u >> 32) as u32 & 0x7fff_ffff;
    let sign = u >> 63 != 0;

    // tgamma(NaN) is NaN, tgamma(inf) is inf, and tgamma(-inf) is NaN with
    // invalid.
    if ix >= 0x7ff0_0000 {
        return x + f64::INFINITY;
    }
    if ix < (0x3ff - 54) << 20 {
        // |x| < 2^-54: tgamma(x) ~ 1/x, and ±0 raises divide-by-zero.
        return 1.0 / x;
    }

    // Integer arguments. floor raises inexact for the others.
    if x == floor(x) {
        if sign {
            return barrier(0.0) / 0.0;
        }
        if x <= FACT.len() as f64 {
            // x is an integer from 1 to 23, so the conversion is exact.
            if let Some(&factorial) = FACT.get((trunc_to_i64(x) - 1) as usize) {
                return factorial;
            }
        }
    }

    // x >= 172: inf with overflow. x <= -184: ±0 with underflow.
    if ix >= 0x4067_0000 {
        // |x| >= 184.
        if sign {
            force_evalf((hexf64!("0x1p-126") / x) as f32);
            if floor(x) * 0.5 == floor(x * 0.5) {
                return 0.0;
            }
            return -0.0;
        }
        return x * hexf64!("0x1p1023");
    }

    let absx = if sign { -x } else { x };

    // The error of x + g - 1/2.
    let y = absx + GMHALF;
    let mut dy = if absx > GMHALF {
        y - absx - GMHALF
    } else {
        y - GMHALF - absx
    };

    let mut z = absx - 0.5;
    let mut r = lanczos_sum(absx) * exp(-y);
    if x < 0.0 {
        // Reflection for a negative x. sinpi(absx) is not 0: the integers are
        // handled.
        r = -PI / (sinpi(absx) * absx * r);
        dy = -dy;
        z = -z;
    }
    r += dy * (GMHALF + 0.5) * r / y;
    z = pow(y, 0.5 * z);
    r * z * z
}

/// [`tgamma`] for `float`: `tgamma`'s result, rounded.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tgammaf(x: f32) -> f32 {
    tgamma(f64::from(x)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn tgamma_matches_libc_test() {
        let files = ["sanity/tgamma.h", "special/tgamma.h"];
        // libc-test's `tgamma.c` tolerates an error under 5.5 ulps.
        let rules = Rules::ULP.tolerate(5.5);
        mtest::d_d("tgamma", &files, |x| tgamma(x), rules, &[]);
    }

    #[test]
    fn tgammaf_matches_libc_test() {
        let files = ["sanity/tgammaf.h", "special/tgammaf.h"];
        mtest::d_d("tgammaf", &files, |x| tgammaf(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        let doubles = [
            (PI, "3.141592653589793238462643383279502884"),
            (GMHALF, "5.524680040776729583740234375"),
            (
                SNUM[0],
                "23531376880.410759688572007674451636754734846804940",
            ),
            (
                SNUM[1],
                "42919803642.649098768957899047001988850926355848959",
            ),
            (
                SNUM[2],
                "35711959237.355668049440185451547166705960488635843",
            ),
            (
                SNUM[3],
                "17921034426.037209699919755754458931112671403265390",
            ),
            (
                SNUM[4],
                "6039542586.3520280050642916443072979210699388420708",
            ),
            (
                SNUM[5],
                "1439720407.3117216736632230727949123939715485786772",
            ),
            (
                SNUM[6],
                "248874557.86205415651146038641322942321632125127801",
            ),
            (
                SNUM[7],
                "31426415.585400194380614231628318205362874684987640",
            ),
            (
                SNUM[8],
                "2876370.6289353724412254090516208496135991145378768",
            ),
            (
                SNUM[9],
                "186056.26539522349504029498971604569928220784236328",
            ),
            (
                SNUM[10],
                "8071.6720023658162106380029022722506138218516325024",
            ),
            (
                SNUM[11],
                "210.82427775157934587250973392071336271166969580291",
            ),
            (
                SNUM[12],
                "2.5066282746310002701649081771338373386264310793408",
            ),
            (SDEN[0], "0"),
            (SDEN[1], "39916800"),
            (SDEN[2], "120543840"),
            (SDEN[3], "150917976"),
            (SDEN[4], "105258076"),
            (SDEN[5], "45995730"),
            (SDEN[6], "13339535"),
            (SDEN[7], "2637558"),
            (SDEN[8], "357423"),
            (SDEN[9], "32670"),
            (SDEN[10], "1925"),
            (SDEN[11], "66"),
            (SDEN[12], "1"),
            (FACT[0], "1"),
            (FACT[1], "1"),
            (FACT[2], "2"),
            (FACT[3], "6"),
            (FACT[4], "24"),
            (FACT[5], "120"),
            (FACT[6], "720"),
            (FACT[7], "5040.0"),
            (FACT[8], "40320.0"),
            (FACT[9], "362880.0"),
            (FACT[10], "3628800.0"),
            (FACT[11], "39916800.0"),
            (FACT[12], "479001600.0"),
            (FACT[13], "6227020800.0"),
            (FACT[14], "87178291200.0"),
            (FACT[15], "1307674368000.0"),
            (FACT[16], "20922789888000.0"),
            (FACT[17], "355687428096000.0"),
            (FACT[18], "6402373705728000.0"),
            (FACT[19], "121645100408832000.0"),
            (FACT[20], "2432902008176640000.0"),
            (FACT[21], "51090942171709440000.0"),
            (FACT[22], "1124000727777607680000.0"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
    }
}
