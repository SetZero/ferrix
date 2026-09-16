//! `sin` and `cos`, the kernels that evaluate them within π/4 of zero, and
//! the reduction of an argument by multiples of π/2 that they share.
//!
//! Ported from musl 1.2.5's `sin.c`, `cos.c`, `__sin.c`, `__cos.c`,
//! `__rem_pio2.c` and `__rem_pio2_large.c` (MIT; see [`crate::math`] for the
//! notice). musl took them from FreeBSD's msun; `__rem_pio2.c` was optimized
//! by Bruce D. Evans. The msun files carry this notice:
//!
//! ```text
//! Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//!
//! Developed at SunPro, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ```
//!
//! musl writes these constants in decimal, with their bits in a comment. They
//! are written here as those bits.
//!
//! # Differences from musl
//!
//! * `__rem_pio2_large` is kept for single and double precision only, which is
//!   what `sin`, `cos` and their `float` forms need. Its table of the bits of
//!   2/π holds the 66 entries those need; musl's longer table serves the
//!   extended and quadruple precisions of `long double`.
//! * It calls `scalbn` and `floor`, which the library does not export yet, so
//!   this module keeps private copies of musl's.
//! * Where musl's C would read past an array if its assumptions failed, the
//!   loops here stop at the array's end instead.

use crate::math::support::{barrier, force_eval, hexf64, high_word};

// __sin.c: the odd polynomial for sin(x) on [0, π/4].
const S1: f64 = f64::from_bits(0xbfc5_5555_5555_5549);
const S2: f64 = f64::from_bits(0x3f81_1111_1110_f8a6);
const S3: f64 = f64::from_bits(0xbf2a_01a0_19c1_61d5);
const S4: f64 = f64::from_bits(0x3ec7_1de3_57b1_fe7d);
const S5: f64 = f64::from_bits(0xbe5a_e5e6_8a2b_9ceb);
const S6: f64 = f64::from_bits(0x3de5_d93a_5acf_d57c);

// __cos.c: the even polynomial for cos(x) on [0, π/4].
const C1: f64 = f64::from_bits(0x3fa5_5555_5555_554c);
const C2: f64 = f64::from_bits(0xbf56_c16c_16c1_5177);
const C3: f64 = f64::from_bits(0x3efa_01a0_19cb_1590);
const C4: f64 = f64::from_bits(0xbe92_7e4f_809c_52ad);
const C5: f64 = f64::from_bits(0x3e21_ee9e_bdb4_b1c4);
const C6: f64 = f64::from_bits(0xbda8_fae9_be88_38d4);

// __rem_pio2.c.
/// `1.5/DBL_EPSILON`: adding and subtracting it rounds to an integer.
const TOINT: f64 = hexf64!("0x1.8p52");
/// π/4.
const PIO4: f64 = hexf64!("0x1.921fb54442d18p-1");
/// 53 bits of 2/π.
const INVPIO2: f64 = f64::from_bits(0x3fe4_5f30_6dc9_c883);
/// The first 33 bits of π/2.
const PIO2_1: f64 = f64::from_bits(0x3ff9_21fb_5440_0000);
/// π/2 - `PIO2_1`.
const PIO2_1T: f64 = f64::from_bits(0x3dd0_b461_1a62_6331);
/// The second 33 bits of π/2.
const PIO2_2: f64 = f64::from_bits(0x3dd0_b461_1a60_0000);
/// π/2 - (`PIO2_1` + `PIO2_2`).
const PIO2_2T: f64 = f64::from_bits(0x3ba3_198a_2e03_7073);
/// The third 33 bits of π/2.
const PIO2_3: f64 = f64::from_bits(0x3ba3_198a_2e00_0000);
/// π/2 - (`PIO2_1` + `PIO2_2` + `PIO2_3`).
const PIO2_3T: f64 = f64::from_bits(0x397b_839a_2520_49c1);

// __rem_pio2_large.c.
/// `init_jk`: one less than the number of terms of 2/π a first pass uses, for
/// single and double precision.
const INIT_JK: [i32; 2] = [3, 4];

/// `ipio2`: 2/π, 24 bits an entry, as far as double precision needs.
const IPIO2: [i32; 66] = [
    0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, //
    0x95993C, 0x439041, 0xFE5163, 0xABDEBB, 0xC561B7, 0x246E3A, //
    0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129, //
    0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, //
    0x3991D6, 0x398353, 0x39F49C, 0x845F8B, 0xBDF928, 0x3B1FF8, //
    0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF, //
    0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, //
    0xF17B3D, 0x0739F7, 0x8A5292, 0xEA6BFB, 0x5FB11F, 0x8D5D08, //
    0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3, //
    0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, //
    0x4D7327, 0x310606, 0x1556CA, 0x73A8C9, 0x60E27B, 0xC08C6B, //
];

/// `PIo2`: π/2 cut into pieces of 24 bits.
const PIO2: [f64; 8] = [
    f64::from_bits(0x3ff9_21fb_4000_0000),
    f64::from_bits(0x3e74_442d_0000_0000),
    f64::from_bits(0x3cf8_4698_8000_0000),
    f64::from_bits(0x3b78_cc51_6000_0000),
    f64::from_bits(0x39f0_1b83_8000_0000),
    f64::from_bits(0x387a_2520_4000_0000),
    f64::from_bits(0x36e3_8222_8000_0000),
    f64::from_bits(0x3569_f31d_0000_0000),
];

/// The sine of `x`, in radians.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sin(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;

    // |x| ~<= π/4.
    if ix <= 0x3fe9_21fb {
        if ix < 0x3e50_0000 {
            // |x| < 2^-26: raise inexact if x is not zero, and underflow if it
            // is subnormal.
            if ix < 0x0010_0000 {
                force_eval(x / hexf64!("0x1p120"));
            } else {
                force_eval(x + hexf64!("0x1p120"));
            }
            return x;
        }
        return kernel_sin(x, 0.0, false);
    }

    // sin(Inf or NaN) is NaN.
    if ix >= 0x7ff0_0000 {
        return barrier(x) - x;
    }

    let (n, y0, y1) = rem_pio2(x);
    match n & 3 {
        0 => kernel_sin(y0, y1, true),
        1 => kernel_cos(y0, y1),
        2 => -kernel_sin(y0, y1, true),
        _ => -kernel_cos(y0, y1),
    }
}

/// The cosine of `x`, in radians.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cos(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;

    // |x| ~<= π/4.
    if ix <= 0x3fe9_21fb {
        if ix < 0x3e46_a09e {
            // |x| < 2^-27·√2: raise inexact if x is not zero.
            force_eval(x + hexf64!("0x1p120"));
            return 1.0;
        }
        return kernel_cos(x, 0.0);
    }

    // cos(Inf or NaN) is NaN.
    if ix >= 0x7ff0_0000 {
        return barrier(x) - x;
    }

    let (n, y0, y1) = rem_pio2(x);
    match n & 3 {
        0 => kernel_cos(y0, y1),
        1 => -kernel_sin(y0, y1, true),
        2 => -kernel_cos(y0, y1),
        _ => kernel_sin(y0, y1, true),
    }
}

/// musl's `__sin`: sin(x + y) for |x| ~<= π/4, where `y` is the tail of `x`
/// and is read only if `tail` is set. The caller returns sin(-0) itself.
pub(crate) fn kernel_sin(x: f64, y: f64, tail: bool) -> f64 {
    let z = x * x;
    let w = z * z;
    let r = S2 + z * (S3 + z * S4) + z * w * (S5 + z * S6);
    let v = z * x;
    if tail {
        // LLVM would rewrite `a - v * S1` as `a + v * -S1`, which rounds the
        // product the other way when rounding up or down. See `crate::math`.
        x - ((z * (0.5 * y - v * r) - y) - barrier(v * S1))
    } else {
        x + v * (S1 + z * r)
    }
}

/// musl's `__cos`: cos(x + y) for |x| ~<= π/4, where `y` is the tail of `x`.
pub(crate) fn kernel_cos(x: f64, y: f64) -> f64 {
    let z = x * x;
    let w = z * z;
    let r = z * (C1 + z * (C2 + z * C3)) + w * w * (C4 + z * (C5 + z * C6));
    let hz = 0.5 * z;
    let w = 1.0 - hz;
    w + (((1.0 - w) - hz) + (z * r - x * y))
}

/// musl's `__rem_pio2`: `x` less the nearest multiple n·π/2, as the sum of a
/// head and a tail, and the n, whose last two bits are what the callers need.
/// Returns `(n, head, tail)`. The caller handles |x| ~<= π/4 itself.
pub(crate) fn rem_pio2(x: f64) -> (i32, f64, f64) {
    let bits = x.to_bits();
    let negative = bits >> 63 != 0;
    let ix = high_word(x) & 0x7fff_ffff;

    // |x| ~<= 5π/4.
    if ix <= 0x400f_6a7a {
        // |x| ~= π/2 or 2π/2: cancellation, which the medium case handles.
        if ix & 0xf_ffff == 0x9_21fb {
            return medium(x, ix);
        }
        // |x| ~<= 3π/4.
        if ix <= 0x4002_d97c {
            return small(x, negative, 1);
        }
        return small(x, negative, 2);
    }
    // |x| ~<= 9π/4.
    if ix <= 0x401c_463b {
        // |x| ~<= 7π/4.
        if ix <= 0x4015_fdbc {
            // |x| ~= 3π/2.
            if ix == 0x4012_d97c {
                return medium(x, ix);
            }
            return small(x, negative, 3);
        }
        // |x| ~= 4π/2.
        if ix == 0x4019_21fb {
            return medium(x, ix);
        }
        return small(x, negative, 4);
    }
    // |x| ~< 2^20·π/2.
    if ix < 0x4139_21fb {
        return medium(x, ix);
    }

    // x is infinite or NaN.
    if ix >= 0x7ff0_0000 {
        let nan = barrier(x) - x;
        return (0, nan, nan);
    }

    // z = scalbn(|x|, -ilogb(x) + 23), cut into three 24-bit integers.
    let mut z = f64::from_bits((bits & (u64::MAX >> 12)) | ((0x3ff_u64 + 23) << 52));
    let t0 = f64::from(z as i32);
    z = (z - t0) * hexf64!("0x1p24");
    let t1 = f64::from(z as i32);
    z = (z - t1) * hexf64!("0x1p24");
    // Skip zero terms; the first is not zero.
    let nx = if z != 0.0 {
        3
    } else if t1 != 0.0 {
        2
    } else {
        1
    };
    let e0 = (ix >> 20) as i32 - (0x3ff + 23);
    let (n, [y0, y1]) = rem_pio2_large(&[t0, t1, z], nx, e0, 1);
    if negative {
        (-n, -y0, -y1)
    } else {
        (n, y0, y1)
    }
}

/// `x` less `k`·π/2 for `k` from 1 to 4, towards zero, with one round of
/// π/2's bits, which is good to 85 bits.
fn small(x: f64, negative: bool, k: i32) -> (i32, f64, f64) {
    // 3·pio2_1t is inexact, so musl computes it at run time.
    let (head, tail) = match k {
        1 => (PIO2_1, PIO2_1T),
        2 => (2.0 * PIO2_1, 2.0 * PIO2_1T),
        3 => (3.0 * PIO2_1, 3.0 * barrier(PIO2_1T)),
        _ => (4.0 * PIO2_1, 4.0 * PIO2_1T),
    };
    if negative {
        let z = x + head;
        let y0 = z + tail;
        (-k, y0, (z - y0) + tail)
    } else {
        let z = x - head;
        let y0 = z - tail;
        (k, y0, (z - y0) - tail)
    }
}

/// The reduction of a medium-sized `x`, whose high word less its sign is
/// `ix`: n is rint(x/(π/2)), and up to three rounds of π/2's bits follow.
fn medium(x: f64, ix: u32) -> (i32, f64, f64) {
    let mut fnv = x * INVPIO2 + TOINT - TOINT;
    let mut n = fnv as i32;
    let mut r = x - fnv * PIO2_1;
    // The first round, good to 85 bits.
    let mut w = fnv * PIO2_1T;
    // This matters with directed rounding.
    if r - w < -PIO4 {
        n -= 1;
        fnv -= 1.0;
        r = x - fnv * PIO2_1;
        w = fnv * PIO2_1T;
    } else if r - w > PIO4 {
        n += 1;
        fnv += 1.0;
        r = x - fnv * PIO2_1;
        w = fnv * PIO2_1T;
    }
    let mut y0 = r - w;
    let ex = (ix >> 20) as i32;
    let mut ey = (y0.to_bits() >> 52 & 0x7ff) as i32;
    if ex - ey > 16 {
        // The second round, good to 118 bits.
        let t = r;
        w = fnv * PIO2_2;
        r = t - w;
        w = fnv * PIO2_2T - ((t - r) - w);
        y0 = r - w;
        ey = (y0.to_bits() >> 52 & 0x7ff) as i32;
        if ex - ey > 49 {
            // The third round, good to 151 bits, which covers every case.
            let t = r;
            w = fnv * PIO2_3;
            r = t - w;
            w = fnv * PIO2_3T - ((t - r) - w);
            y0 = r - w;
        }
    }
    (n, y0, (r - y0) - w)
}

/// Element `index` of `array`, or the default if there is none.
fn at<T: Copy + Default>(array: &[T], index: i32) -> T {
    usize::try_from(index)
        .ok()
        .and_then(|index| array.get(index))
        .copied()
        .unwrap_or_default()
}

/// Sets element `index` of `array`, if there is one.
fn put<T>(array: &mut [T], index: i32, value: T) {
    if let Some(slot) = usize::try_from(index)
        .ok()
        .and_then(|index| array.get_mut(index))
    {
        *slot = value;
    }
}

/// Replaces element `index` of `array`, if there is one, with `change` of it.
fn update<T: Copy>(array: &mut [T], index: i32, change: impl FnOnce(T) -> T) {
    if let Some(slot) = usize::try_from(index)
        .ok()
        .and_then(|index| array.get_mut(index))
    {
        *slot = change(*slot);
    }
}

/// The sum over j of x[j]·f[jx + i - j]: the ith 24-bit piece of x·(2/π).
fn term(x: &[f64; 3], f: &[f64; 20], jx: i32, i: i32) -> f64 {
    let mut fw = 0.0;
    for j in 0..=jx {
        fw += at(x, j) * at(f, jx + i - j);
    }
    fw
}

/// musl's `__rem_pio2_large`: the last three bits of n and the remainder
/// y = x - n·π/2, |y| < π/2, for a positive `x` given as its first `nx` pieces
/// of 24 bits, the first scaled by 2^`e0`.
///
/// `prec` is 0 for single precision, which returns the remainder in one
/// `double`, and 1 for double precision, which returns a head and a tail.
pub(crate) fn rem_pio2_large(x: &[f64; 3], nx: i32, e0: i32, prec: i32) -> (i32, [f64; 2]) {
    let jk = at(&INIT_JK, prec);
    let jp = jk;
    let jx = nx - 1;
    let jv = ((e0 - 3) / 24).max(0);
    let mut q0 = e0 - 24 * (jv + 1);

    let mut f = [0.0f64; 20];
    let mut q = [0.0f64; 20];
    let mut fq = [0.0f64; 20];
    let mut iq = [0i32; 20];

    // f[0] to f[jx + jk], where f[jx + jk] = ipio2[jv + jk].
    for i in 0..=jx + jk {
        let j = jv - jx + i;
        put(
            &mut f,
            i,
            if j < 0 { 0.0 } else { f64::from(at(&IPIO2, j)) },
        );
    }

    // q[0] to q[jk].
    for i in 0..=jk {
        put(&mut q, i, term(x, &f, jx, i));
    }

    let mut jz = jk;
    let (mut z, n, ih) = loop {
        // Distill q[] into iq[], in reverse.
        let mut z = at(&q, jz);
        let mut i = 0;
        let mut j = jz;
        while j > 0 {
            let fw = f64::from((hexf64!("0x1p-24") * z) as i32);
            put(&mut iq, i, (z - hexf64!("0x1p24") * fw) as i32);
            z = at(&q, j - 1) + fw;
            i += 1;
            j -= 1;
        }

        // n.
        z = scalbn(z, q0);
        z -= 8.0 * floor(z * 0.125);
        let mut n = z as i32;
        z -= f64::from(n);
        let mut ih = 0;
        if q0 > 0 {
            // iq[jz - 1] is needed to determine n.
            let i = at(&iq, jz - 1) >> (24 - q0);
            n += i;
            update(&mut iq, jz - 1, |value| value - (i << (24 - q0)));
            ih = at(&iq, jz - 1) >> (23 - q0);
        } else if q0 == 0 {
            ih = at(&iq, jz - 1) >> 23;
        } else if z >= 0.5 {
            ih = 2;
        }

        // q > 0.5.
        if ih > 0 {
            n += 1;
            let mut carry = 0;
            // 1 - q.
            for i in 0..jz {
                let j = at(&iq, i);
                if carry != 0 {
                    put(&mut iq, i, 0xff_ffff - j);
                } else if j != 0 {
                    carry = 1;
                    put(&mut iq, i, 0x100_0000 - j);
                }
            }
            // The rare case, one in twelve.
            match q0 {
                1 => update(&mut iq, jz - 1, |value| value & 0x7f_ffff),
                2 => update(&mut iq, jz - 1, |value| value & 0x3f_ffff),
                _ => {}
            }
            if ih == 2 {
                z = 1.0 - z;
                if carry != 0 {
                    z -= scalbn(1.0, q0);
                }
            }
        }

        // Recompute with more terms if the fraction cancelled.
        if z == 0.0 {
            let mut j = 0;
            let mut i = jz - 1;
            while i >= jk {
                j |= at(&iq, i);
                i -= 1;
            }
            if j == 0 {
                // k is the number of terms needed.
                let mut k = 1;
                while k <= jk && at(&iq, jk - k) == 0 {
                    k += 1;
                }
                // Add q[jz + 1] to q[jz + k].
                for i in jz + 1..=jz + k {
                    put(&mut f, jx + i, f64::from(at(&IPIO2, jv + i)));
                    put(&mut q, i, term(x, &f, jx, i));
                }
                jz += k;
                continue;
            }
        }
        break (z, n, ih);
    };

    if z == 0.0 {
        // Chop off zero terms.
        jz -= 1;
        q0 -= 24;
        while jz > 0 && at(&iq, jz) == 0 {
            jz -= 1;
            q0 -= 24;
        }
    } else {
        // Break z into 24-bit pieces if necessary.
        z = scalbn(z, -q0);
        if z >= hexf64!("0x1p24") {
            let fw = f64::from((hexf64!("0x1p-24") * z) as i32);
            put(&mut iq, jz, (z - hexf64!("0x1p24") * fw) as i32);
            jz += 1;
            q0 += 24;
            put(&mut iq, jz, fw as i32);
        } else {
            put(&mut iq, jz, z as i32);
        }
    }

    // The integer pieces as floating-point values.
    let mut fw = scalbn(1.0, q0);
    let mut i = jz;
    while i >= 0 {
        put(&mut q, i, fw * f64::from(at(&iq, i)));
        fw *= hexf64!("0x1p-24");
        i -= 1;
    }

    // PIo2[0..=jp]·q[jz..=0].
    let mut i = jz;
    while i >= 0 {
        let mut fw = 0.0;
        let mut k = 0;
        while k <= jp && k <= jz - i {
            fw += at(&PIO2, k) * at(&q, i + k);
            k += 1;
        }
        put(&mut fq, jz - i, fw);
        i -= 1;
    }

    // Compress fq[] into y[].
    let mut fw = 0.0;
    let mut i = jz;
    while i >= 0 {
        fw += at(&fq, i);
        i -= 1;
    }
    let head = if ih == 0 { fw } else { -fw };
    if prec == 0 {
        return (n & 7, [head, 0.0]);
    }
    fw = at(&fq, 0) - fw;
    for i in 1..=jz {
        fw += at(&fq, i);
    }
    (n & 7, [head, if ih == 0 { fw } else { -fw }])
}

/// `x`·2^`n`, as musl's `scalbn` computes it.
fn scalbn(x: f64, mut n: i32) -> f64 {
    let mut y = x;
    if n > 1023 {
        y *= hexf64!("0x1p1023");
        n -= 1023;
        if n > 1023 {
            y *= hexf64!("0x1p1023");
            n -= 1023;
            if n > 1023 {
                n = 1023;
            }
        }
    } else if n < -1022 {
        // Make sure the final n is below -53, to avoid rounding twice in the
        // subnormal range.
        y *= hexf64!("0x1p-1022") * hexf64!("0x1p53");
        n += 1022 - 53;
        if n < -1022 {
            y *= hexf64!("0x1p-1022") * hexf64!("0x1p53");
            n += 1022 - 53;
            if n < -1022 {
                n = -1022;
            }
        }
    }
    // n is now in [-1022, 1023], so the biased exponent is positive.
    y * f64::from_bits(u64::from((0x3ff + n).unsigned_abs()) << 52)
}

/// The largest integer not greater than `x`, as musl's `floor` computes it.
fn floor(x: f64) -> f64 {
    /// `1/DBL_EPSILON`.
    const TOINT: f64 = hexf64!("0x1p52");
    let bits = x.to_bits();
    let e = (bits >> 52 & 0x7ff) as i32;
    if e >= 0x3ff + 52 || x == 0.0 {
        return x;
    }
    // y = int(x) - x, where int(x) is an integer neighbour of x.
    let y = if bits >> 63 != 0 {
        x - TOINT + TOINT - x
    } else {
        x + TOINT - TOINT - x
    };
    // A special case because of rounding modes other than to nearest.
    if e < 0x3ff {
        force_eval(y);
        return if bits >> 63 != 0 { -1.0 } else { 0.0 };
    }
    if y > 0.0 { x + y - 1.0 } else { x + y }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    /// libc-test's `sin.c` and `cos.c` count an error in a rounding mode other
    /// than to nearest as tolerated.
    const RULES: Rules = Rules::ULP.directed();

    #[test]
    fn sin_matches_libc_test() {
        let files = ["crlibm/sin.h", "ucb/sin.h", "sanity/sin.h", "special/sin.h"];
        mtest::d_d("sin", &files, |x| sin(x), RULES, &[]);
    }

    #[test]
    fn cos_matches_libc_test() {
        let files = ["crlibm/cos.h", "ucb/cos.h", "sanity/cos.h", "special/cos.h"];
        mtest::d_d("cos", &files, |x| cos(x), RULES, &[]);
    }

    #[test]
    fn sin_rounds_as_musl_does_upward_and_downward() {
        use crate::math::mtest::fenv::{FE_DOWNWARD, FE_INEXACT, FE_UPWARD};
        // musl 1.2.5's results. Each differed by an ulp in the release build
        // while LLVM could turn `a - v * S1` into `a + v * -S1`.
        let cases = [
            (
                FE_DOWNWARD,
                hexf64!("-0x1.3773fbd46be7dp+3"),
                hexf64!("0x1.368e5c4f62bfap-2"),
            ),
            (
                FE_UPWARD,
                hexf64!("-0x1.394540722b124p+1"),
                hexf64!("-0x1.478cb03f39e3bp-1"),
            ),
            (
                FE_DOWNWARD,
                hexf64!("-0x1.3954425bfd23dp+204"),
                hexf64!("-0x1.3f95241253e1p-2"),
            ),
            (
                FE_UPWARD,
                hexf64!("-0x1.5ea04e4bf8851p+9"),
                hexf64!("0x1.40d64b11aedd8p-1"),
            ),
        ];
        for (mode, x, want) in cases {
            let (got, raised) = mtest::under(mode, || sin(x));
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "sin({x:e}) in mode {mode:#x}"
            );
            assert_eq!(raised, FE_INEXACT, "sin({x:e}) in mode {mode:#x}");
        }
    }

    #[test]
    fn scalbn_and_floor_are_musls() {
        assert_eq!(scalbn(1.0, -1074).to_bits(), 1);
        assert_eq!(scalbn(1.5, 1023), hexf64!("0x1.8p1023"));
        assert_eq!(scalbn(1.0, 2000), f64::INFINITY);
        assert_eq!(scalbn(3.0, -3), 0.375);
        assert_eq!(floor(-0.5), -1.0);
        assert_eq!(floor(0.5).to_bits(), 0);
        assert_eq!(floor(7.999), 7.0);
        assert_eq!(floor(-7.001), -8.0);
        assert_eq!(floor(hexf64!("0x1p60")), hexf64!("0x1p60"));
    }
}
