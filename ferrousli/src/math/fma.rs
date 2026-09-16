//! `fma` and `fmaf`: `x × y + z` with a single rounding, in the current mode,
//! without a hardware fused multiply-add. SSE2, the baseline, has none.
//!
//! `fma` is ported from musl 1.2.5's `fma.c` (MIT; see [`crate::math`] for
//! the notice), which multiplies the significands exactly into 128 bits, adds
//! `z` aligned with a sticky bit, and rounds once when converting to `double`.
//! One case differs from musl: see [`fma`].
//!
//! `fmaf` is not musl's. musl's `fmaf.c`, from FreeBSD, adds in `double` and
//! corrects only a result exactly halfway between two normal `float`s. In the
//! `float` subnormal range the `double` sum can round across a `float`
//! rounding boundary, and five of libc-test's cases get the wrong result, as
//! they do with musl. Here the sum is exact, computed in integers; see
//! [`fmaf`].

use crate::math::manipulate::scalbn;
use crate::math::support::{force_eval, hexf64};

/// The exponent [`normalize`] gives an infinity or a NaN. A zero's is larger.
const ZERO_INF_NAN: i32 = 0x7ff - 0x3ff - 52 - 1;

/// A `double` as `±m × 2^e`.
#[derive(Debug, Clone, Copy)]
struct Num {
    /// The significand, with its top ten bits and its last bit clear.
    m: u64,
    /// The exponent.
    e: i32,
    /// Whether the sign is negative.
    negative: bool,
}

/// `x` as `±m × 2^e`, with a subnormal normalised. A zero's exponent is
/// above [`ZERO_INF_NAN`], and an infinity's or a NaN's is equal to it.
fn normalize(x: f64) -> Num {
    let mut bits = x.to_bits();
    let negative = bits >> 63 != 0;
    let mut e = (bits >> 52 & 0x7ff) as i32;
    if e == 0 {
        // The product is exact.
        bits = (x * hexf64!("0x1p63")).to_bits();
        e = (bits >> 52 & 0x7ff) as i32;
        e = if e != 0 { e - 63 } else { 0x800 };
    }
    bits &= (1 << 52) - 1;
    bits |= 1 << 52;
    bits <<= 1;
    Num {
        m: bits,
        e: e - (0x3ff + 52 + 1),
        negative,
    }
}

/// `x × y + z`, correctly rounded in the current rounding mode.
///
/// When `z` is a zero and `x × y` is not, the result is the product, rounded
/// once. musl returns `x * y + z` there, which is +0 to nearest when the
/// product underflows to -0 and `z` is +0; the exact result is negative, so
/// the right answer is -0.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fma(x: f64, y: f64, z: f64) -> f64 {
    let nx = normalize(x);
    let ny = normalize(y);
    let nz = normalize(z);

    if nx.e >= ZERO_INF_NAN || ny.e >= ZERO_INF_NAN {
        return x * y + z;
    }
    if nz.e >= ZERO_INF_NAN {
        if nz.e > ZERO_INF_NAN {
            // z is zero, and x and y are finite and nonzero.
            return x * y;
        }
        return z;
    }

    // r = x × y, exactly. Either the top 20 or 21 bits of rhi, and the last
    // two bits of rlo, are zero.
    let product = u128::from(nx.m) * u128::from(ny.m);
    let mut rhi = (product >> 64) as u64;
    let mut rlo = product as u64;

    // Align the exponents: shift z left by kz and r right by kr, so that
    // kz + kr == d, and set e = e + kr, which is nz.e - kz.
    let mut e = nx.e + ny.e;
    let mut d = nz.e - e;
    let zhi;
    let zlo;
    if d > 0 {
        if d < 64 {
            zlo = nz.m << d;
            zhi = nz.m >> (64 - d);
        } else {
            zlo = 0;
            zhi = nz.m;
            e = nz.e - 64;
            d -= 64;
            if d == 0 {
                // Aligned already.
            } else if d < 64 {
                rlo = rhi << (64 - d) | rlo >> d | u64::from(rlo << (64 - d) != 0);
                rhi >>= d;
            } else {
                rlo = 1;
                rhi = 0;
            }
        }
    } else {
        zhi = 0;
        d = -d;
        zlo = if d == 0 {
            nz.m
        } else if d < 64 {
            nz.m >> d | u64::from(nz.m << (64 - d) != 0)
        } else {
            1
        };
    }

    // Add.
    let mut negative = nx.negative != ny.negative;
    let same_sign = negative == nz.negative;
    let mut nonzero = true;
    if same_sign {
        // r += z
        rlo = rlo.wrapping_add(zlo);
        rhi = rhi.wrapping_add(zhi).wrapping_add(u64::from(rlo < zlo));
    } else {
        // r -= z
        let t = rlo;
        rlo = rlo.wrapping_sub(zlo);
        rhi = rhi.wrapping_sub(zhi).wrapping_sub(u64::from(t < rlo));
        if rhi >> 63 != 0 {
            rlo = rlo.wrapping_neg();
            rhi = rhi.wrapping_neg().wrapping_sub(u64::from(rlo != 0));
            negative = !negative;
        }
        nonzero = rhi != 0;
    }

    // Set rhi to the top 63 bits of the result; the last bit is sticky.
    if nonzero {
        e += 64;
        // At least the top bit of rhi is clear, so d > 0.
        d = rhi.leading_zeros() as i32 - 1;
        rhi = rhi << d | rlo >> (64 - d) | u64::from(rlo << d != 0);
    } else if rlo != 0 {
        d = rlo.leading_zeros() as i32 - 1;
        rhi = if d < 0 {
            rlo >> 1 | (rlo & 1)
        } else {
            rlo << d
        };
    } else {
        // An exact ±0.
        return x * y + z;
    }
    e -= d;

    // Convert to double. i is in [2^62, 2^63 - 1], so it fits and negates.
    let mut i = rhi as i64;
    if negative {
        i = -i;
    }
    // The conversion rounds in the current mode: |r| is in [2^62, 2^63].
    let mut r = i as f64;

    if e < -1022 - 62 {
        // The result is subnormal before rounding.
        if e == -1022 - 63 {
            let c = if negative {
                -hexf64!("0x1p63")
            } else {
                hexf64!("0x1p63")
            };
            if r == c {
                // The smallest normal after rounding. Whether underflow is
                // raised depends on the architecture, which a conversion from
                // double to float imitates.
                let fltmin = (hexf64!("0x0.ffffff8p-63") * hexf64!("0x1p-126") * r) as f32;
                return hexf64!("0x1p-896") * f64::from(fltmin);
            }
            // One bit is lost when scaled: add another top bit, so that an
            // inexact result is rounded only once, at the conversion.
            if rhi << 53 != 0 {
                i = (rhi >> 1 | (rhi & 1) | 1 << 62) as i64;
                if negative {
                    i = -i;
                }
                r = i as f64;
                // Remove the top bit.
                r = 2.0 * r - c;
                // Raise underflow in a way the optimiser cannot remove.
                let tiny = hexf64!("0x1p-896") * r;
                force_eval(tiny * tiny);
            }
        } else {
            // Round only once, when scaled.
            let d = 10;
            i = ((rhi >> d | u64::from(rhi << (64 - d) != 0)) << d) as i64;
            if negative {
                i = -i;
            }
            r = i as f64;
        }
    }
    scalbn(r, e)
}

/// A finite nonzero `float` as `m × 2^e` with `m`'s top bit at bit 63, and
/// its sign.
fn float_parts(x: f32) -> (u64, i32, bool) {
    let bits = x.to_bits();
    let biased = (bits >> 23 & 0xff) as i32;
    let fraction = u64::from(bits & 0x007f_ffff);
    let (m, e) = if biased == 0 {
        (fraction, -149)
    } else {
        (fraction | 1 << 23, biased - 150)
    };
    let shift = m.leading_zeros() as i32;
    (m << shift, e - shift, bits >> 31 != 0)
}

/// Whether `x` is finite and nonzero, tested on its bits.
fn ordinary(x: f32) -> bool {
    let magnitude = x.to_bits() << 1;
    magnitude != 0 && magnitude < 0xff << 24
}

/// `x × y + z` for `float`, correctly rounded in the current rounding mode.
///
/// The sum is computed exactly in integers, as `S × 2^e`, with bits lost only
/// when `z` and the product are more than 64 binary orders of magnitude apart;
/// those fold into a sticky bit. `S` is then cut to a `double`'s 53 bits, again
/// with a sticky bit. Every `float` rounding boundary lies many bits above
/// either sticky bit, so the `double` rounds to the same `float` as the exact
/// sum in every mode, and the one conversion at the end raises inexact,
/// underflow and overflow as the exact operation would.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fmaf(x: f32, y: f32, z: f32) -> f32 {
    if !(ordinary(x) && ordinary(y) && ordinary(z)) {
        // With a zero, an infinity or a NaN among them, the double operations
        // are exact, and raise what the float ones would.
        return (f64::from(x) * f64::from(y) + f64::from(z)) as f32;
    }
    let (mx, ex, x_negative) = float_parts(x);
    let (my, ey, y_negative) = float_parts(y);
    let (mz, ez, z_negative) = float_parts(z);

    // The product. Each factor has at most 24 significant bits, so the low 64
    // bits of the 128-bit product are zero, and its top bit is bit 126 or 127.
    let product = ((u128::from(mx) * u128::from(my)) >> 64) as u64;
    let shift = product.leading_zeros() as i32;
    let (mxy, exy) = (product << shift, ex + ey + 64 - shift);
    let xy_negative = x_negative != y_negative;

    // The terms, larger exponent first.
    let ((big, big_e, big_negative), (small, small_e, small_negative)) = if exy >= ez {
        ((mxy, exy, xy_negative), (mz, ez, z_negative))
    } else {
        ((mz, ez, z_negative), (mxy, exy, xy_negative))
    };
    let d = big_e - small_e;
    // Align at a common exponent e. Within 64 orders the alignment is exact;
    // beyond, the smaller term's bits below the unit fold into a sticky bit.
    let (b, s, e) = if d <= 64 {
        (u128::from(big) << d, u128::from(small), small_e)
    } else {
        let s = if d >= 128 {
            1
        } else {
            let k = d - 64;
            small >> k | u64::from(small << (64 - k) != 0)
        };
        (u128::from(big) << 64, u128::from(s), big_e - 64)
    };
    let (magnitude, negative) = if big_negative == small_negative {
        (b + s, big_negative)
    } else if b >= s {
        (b - s, big_negative)
    } else {
        (s - b, small_negative)
    };
    if magnitude == 0 {
        // An exact zero, whose sign depends on the rounding mode. The double
        // sum is exact too, and has that sign.
        return (f64::from(x) * f64::from(y) + f64::from(z)) as f32;
    }

    // The top 53 bits, with a sticky bit, as a double's significand.
    let top = 127 - magnitude.leading_zeros() as i32;
    let significand = if top > 52 {
        let k = top - 52;
        let lost = magnitude & ((1 << k) - 1) != 0;
        (magnitude >> k) as u64 | u64::from(lost)
    } else {
        (magnitude << (52 - top)) as u64
    };
    // The sum is between 2^-300 and 2^260, so the exponent is a normal one.
    let biased = (e + top + 0x3ff) as u64;
    let bits = u64::from(negative) << 63 | biased << 52 | significand & ((1 << 52) - 1);
    f64::from_bits(bits) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fenv::{FE_DOWNWARD, FE_TONEAREST, FE_TOWARDZERO, FE_UPWARD};
    use crate::math::mtest::{self, Random, Rules};
    use crate::math::support::hexf32;

    unsafe extern "C" {
        #[link_name = "fma"]
        safe fn host_fma(x: f64, y: f64, z: f64) -> f64;
        #[link_name = "fmaf"]
        safe fn host_fmaf(x: f32, y: f32, z: f32) -> f32;
    }

    /// libc-test's `fma` programs ignore inexact unless built to check it.
    const FMA: Rules = Rules::EXACT.ignore_inexact();

    #[test]
    fn fma_matches_libc_test() {
        let files = ["sanity/fma.h", "special/fma.h"];
        mtest::ddd_d("fma", &files, |x, y, z| fma(x, y, z), FMA, &[]);
        let files = ["sanity/fmaf.h", "special/fmaf.h"];
        mtest::ddd_d("fmaf", &files, |x, y, z| fmaf(x, y, z), FMA, &[]);
    }

    #[test]
    fn fma_rounds_once() {
        // (1 + 2^-52)^2 is 1 + 2^-51 + 2^-104. Rounding the product first
        // would lose the last term, and the result would be zero.
        let x = 1.0 + hexf64!("0x1p-52");
        assert_eq!(fma(x, x, -(1.0 + hexf64!("0x1p-51"))), hexf64!("0x1p-104"));
        // An underflowing product keeps its sign when z is a zero.
        let tiny = hexf64!("0x1p-1000");
        assert_eq!(fma(-tiny, hexf64!("0x1p-100"), 0.0).to_bits(), 1 << 63);
        assert_eq!(fma(-0.0, 0.0, 0.0).to_bits(), 0);
        assert_eq!(fma(hexf64!("0x1p-1074"), 0.5, 0.0), 0.0);
        let third = f32::from_bits(0x3eaa_aaab);
        assert_eq!(fmaf(third, 3.0, -1.0), hexf32!("0x1p-25"));
    }

    /// A number with random bits, an exponent in `[-emax, emax]` and a random
    /// sign, as `double`s spread over the whole range are.
    fn scattered(random: &mut Random, emax: u64) -> f64 {
        let bits = random.bits();
        let exponent = (bits >> 52) % (2 * emax + 1);
        let biased = 0x3ff + exponent - emax;
        f64::from_bits(bits & (1 << 63 | ((1 << 52) - 1)) | biased << 52)
    }

    /// Three numbers whose product and sum are close, so that the sum cancels
    /// and the rounding of every term matters.
    fn cancelling(random: &mut Random, emax: u64) -> (f64, f64, f64) {
        let x = scattered(random, emax);
        let y = scattered(random, emax);
        let near = -(x * y);
        let z = f64::from_bits(near.to_bits() ^ (random.bits() & 0xff));
        (x, y, z)
    }

    #[test]
    fn fma_agrees_exactly_with_glibc_in_every_mode() {
        let mut random = Random::new("fma");
        for mode in [FE_TONEAREST, FE_DOWNWARD, FE_UPWARD, FE_TOWARDZERO] {
            for round in 0..mtest::SAMPLES {
                let (x, y, z) = if round % 2 == 0 {
                    let emax = [20, 520, 1023][round % 3];
                    let x = scattered(&mut random, emax);
                    let y = scattered(&mut random, emax);
                    (x, y, scattered(&mut random, emax))
                } else {
                    cancelling(&mut random, 500)
                };
                let ((ours, theirs), _) = mtest::under(mode, || (fma(x, y, z), host_fma(x, y, z)));
                assert!(
                    mtest::checkcr(ours, theirs),
                    "mode {mode:#x}: fma({x:e}, {y:e}, {z:e}) = {ours:e}, glibc {theirs:e}"
                );
            }
        }
        println!("glibc fma: equal at {} inputs in each mode", mtest::SAMPLES);
    }

    #[test]
    fn fmaf_agrees_exactly_with_glibc_in_every_mode() {
        let mut random = Random::new("fmaf");
        for mode in [FE_TONEAREST, FE_DOWNWARD, FE_UPWARD, FE_TOWARDZERO] {
            for round in 0..mtest::SAMPLES {
                let (x, y, z) = if round % 2 == 0 {
                    let emax = [10, 80, 127][round % 3];
                    let x = scattered(&mut random, emax) as f32;
                    let y = scattered(&mut random, emax) as f32;
                    (x, y, scattered(&mut random, emax) as f32)
                } else {
                    let (x, y, _) = cancelling(&mut random, 60);
                    let (x, y) = (x as f32, y as f32);
                    let near = -(x * y);
                    let z = f32::from_bits(near.to_bits() ^ (random.bits() as u32 & 0xf));
                    (x, y, z)
                };
                let ((ours, theirs), _) =
                    mtest::under(mode, || (fmaf(x, y, z), host_fmaf(x, y, z)));
                assert!(
                    mtest::checkcr(ours, theirs),
                    "mode {mode:#x}: fmaf({x:e}, {y:e}, {z:e}) = {ours:e}, glibc {theirs:e}"
                );
            }
        }
        println!(
            "glibc fmaf: equal at {} inputs in each mode",
            mtest::SAMPLES
        );
    }
}
