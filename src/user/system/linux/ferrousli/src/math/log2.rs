//! `log2`, the base-2 logarithm.
//!
//! Ported from musl 1.2.5's `log2.c`, `log2_data.c` and `log2_data.h` (MIT;
//! see [`crate::math`] for the notice). musl took them from ARM's
//! optimized-routines, whose files carry this notice:
//!
//! ```text
//! Copyright (c) 2018, Arm Limited.
//! SPDX-License-Identifier: MIT
//! ```
//!
//! The tables were converted from `log2_data.c` by a script, digit for digit.
//! musl's x86-64 build has no `__FP_FAST_FMA`, so the code kept is the code
//! without it, and with `tab2`.

use crate::math::log::top16;
use crate::math::support::{divzero, hexf64, invalid};

/// `LOG2_TABLE_BITS`: z's range is split into N = 2^6 subintervals.
const TABLE_BITS: u32 = 6;
/// `N`.
const N: u64 = 1 << TABLE_BITS;
/// The bottom of z's range, [`OFF`, 2·`OFF`).
const OFF: u64 = 0x3fe6_0000_0000_0000;

/// The high bits of 1/ln2.
const INV_LN2_HI: f64 = hexf64!("0x1.7154765200000p+0");
/// The rest of 1/ln2.
const INV_LN2_LO: f64 = hexf64!("0x1.705fc2eefa200p-33");

// `poly`: log2(1 + r) - r/ln2's coefficients from r², for |r| < 1/2N.
const A0: f64 = hexf64!("-0x1.71547652b8339p-1");
const A1: f64 = hexf64!("0x1.ec709dc3a04bep-2");
const A2: f64 = hexf64!("-0x1.7154764702ffbp-2");
const A3: f64 = hexf64!("0x1.2776c50034c48p-2");
const A4: f64 = hexf64!("-0x1.ec7b328ea92bcp-3");
const A5: f64 = hexf64!("0x1.a6225e117f92ep-3");

// `poly1`: the coefficients from r² for x close to 1.
const B0: f64 = hexf64!("-0x1.71547652b82fep-1");
const B1: f64 = hexf64!("0x1.ec709dc3a03f7p-2");
const B2: f64 = hexf64!("-0x1.71547652b7c3fp-2");
const B3: f64 = hexf64!("0x1.2776c50f05be4p-2");
const B4: f64 = hexf64!("-0x1.ec709dd768fe5p-3");
const B5: f64 = hexf64!("0x1.a61761ec4e736p-3");
const B6: f64 = hexf64!("-0x1.7153fbc64a79bp-3");
const B7: f64 = hexf64!("0x1.484d154f01b4ap-3");
const B8: f64 = hexf64!("-0x1.289e4a72c383cp-3");
const B9: f64 = hexf64!("0x1.0b32f285aee66p-3");

/// A table row of two `double`s, written as musl's table writes it.
macro_rules! pair {
    ($a:literal, $b:literal) => {
        (hexf64!($a), hexf64!($b))
    };
}

/// `tab`: 1/c and log2(c) for the c near the middle of each subinterval.
static TABLE: [(f64, f64); N as usize] = [
    pair!("0x1.724286bb1acf8p+0", "-0x1.1095feecdb000p-1"),
    pair!("0x1.6e1f766d2cca1p+0", "-0x1.08494bd76d000p-1"),
    pair!("0x1.6a13d0e30d48ap+0", "-0x1.00143aee8f800p-1"),
    pair!("0x1.661ec32d06c85p+0", "-0x1.efec5360b4000p-2"),
    pair!("0x1.623fa951198f8p+0", "-0x1.dfdd91ab7e000p-2"),
    pair!("0x1.5e75ba4cf026cp+0", "-0x1.cffae0cc79000p-2"),
    pair!("0x1.5ac055a214fb8p+0", "-0x1.c043811fda000p-2"),
    pair!("0x1.571ed0f166e1ep+0", "-0x1.b0b67323ae000p-2"),
    pair!("0x1.53909590bf835p+0", "-0x1.a152f5a2db000p-2"),
    pair!("0x1.5014fed61adddp+0", "-0x1.9217f5af86000p-2"),
    pair!("0x1.4cab88e487bd0p+0", "-0x1.8304db0719000p-2"),
    pair!("0x1.49539b4334feep+0", "-0x1.74189f9a9e000p-2"),
    pair!("0x1.460cbdfafd569p+0", "-0x1.6552bb5199000p-2"),
    pair!("0x1.42d664ee4b953p+0", "-0x1.56b23a29b1000p-2"),
    pair!("0x1.3fb01111dd8a6p+0", "-0x1.483650f5fa000p-2"),
    pair!("0x1.3c995b70c5836p+0", "-0x1.39de937f6a000p-2"),
    pair!("0x1.3991c4ab6fd4ap+0", "-0x1.2baa1538d6000p-2"),
    pair!("0x1.3698e0ce099b5p+0", "-0x1.1d98340ca4000p-2"),
    pair!("0x1.33ae48213e7b2p+0", "-0x1.0fa853a40e000p-2"),
    pair!("0x1.30d191985bdb1p+0", "-0x1.01d9c32e73000p-2"),
    pair!("0x1.2e025cab271d7p+0", "-0x1.e857da2fa6000p-3"),
    pair!("0x1.2b404cf13cd82p+0", "-0x1.cd3c8633d8000p-3"),
    pair!("0x1.288b02c7ccb50p+0", "-0x1.b26034c14a000p-3"),
    pair!("0x1.25e2263944de5p+0", "-0x1.97c1c2f4fe000p-3"),
    pair!("0x1.234563d8615b1p+0", "-0x1.7d6023f800000p-3"),
    pair!("0x1.20b46e33eaf38p+0", "-0x1.633a71a05e000p-3"),
    pair!("0x1.1e2eefdcda3ddp+0", "-0x1.494f5e9570000p-3"),
    pair!("0x1.1bb4a580b3930p+0", "-0x1.2f9e424e0a000p-3"),
    pair!("0x1.19453847f2200p+0", "-0x1.162595afdc000p-3"),
    pair!("0x1.16e06c0d5d73cp+0", "-0x1.f9c9a75bd8000p-4"),
    pair!("0x1.1485f47b7e4c2p+0", "-0x1.c7b575bf9c000p-4"),
    pair!("0x1.12358ad0085d1p+0", "-0x1.960c60ff48000p-4"),
    pair!("0x1.0fef00f532227p+0", "-0x1.64ce247b60000p-4"),
    pair!("0x1.0db2077d03a8fp+0", "-0x1.33f78b2014000p-4"),
    pair!("0x1.0b7e6d65980d9p+0", "-0x1.0387d1a42c000p-4"),
    pair!("0x1.0953efe7b408dp+0", "-0x1.a6f9208b50000p-5"),
    pair!("0x1.07325cac53b83p+0", "-0x1.47a954f770000p-5"),
    pair!("0x1.05197e40d1b5cp+0", "-0x1.d23a8c50c0000p-6"),
    pair!("0x1.03091c1208ea2p+0", "-0x1.16a2629780000p-6"),
    pair!("0x1.0101025b37e21p+0", "-0x1.720f8d8e80000p-8"),
    pair!("0x1.fc07ef9caa76bp-1", "0x1.6fe53b1500000p-7"),
    pair!("0x1.f4465d3f6f184p-1", "0x1.11ccce10f8000p-5"),
    pair!("0x1.ecc079f84107fp-1", "0x1.c4dfc8c8b8000p-5"),
    pair!("0x1.e573a99975ae8p-1", "0x1.3aa321e574000p-4"),
    pair!("0x1.de5d6f0bd3de6p-1", "0x1.918a0d08b8000p-4"),
    pair!("0x1.d77b681ff38b3p-1", "0x1.e72e9da044000p-4"),
    pair!("0x1.d0cb5724de943p-1", "0x1.1dcd2507f6000p-3"),
    pair!("0x1.ca4b2dc0e7563p-1", "0x1.476ab03dea000p-3"),
    pair!("0x1.c3f8ee8d6cb51p-1", "0x1.7074377e22000p-3"),
    pair!("0x1.bdd2b4f020c4cp-1", "0x1.98ede8ba94000p-3"),
    pair!("0x1.b7d6c006015cap-1", "0x1.c0db86ad2e000p-3"),
    pair!("0x1.b20366e2e338fp-1", "0x1.e840aafcee000p-3"),
    pair!("0x1.ac57026295039p-1", "0x1.0790ab4678000p-2"),
    pair!("0x1.a6d01bc2731ddp-1", "0x1.1ac056801c000p-2"),
    pair!("0x1.a16d3bc3ff18bp-1", "0x1.2db11d4fee000p-2"),
    pair!("0x1.9c2d14967feadp-1", "0x1.406464ec58000p-2"),
    pair!("0x1.970e4f47c9902p-1", "0x1.52dbe093af000p-2"),
    pair!("0x1.920fb3982bcf2p-1", "0x1.651902050d000p-2"),
    pair!("0x1.8d30187f759f1p-1", "0x1.771d2cdeaf000p-2"),
    pair!("0x1.886e5ebb9f66dp-1", "0x1.88e9c857d9000p-2"),
    pair!("0x1.83c97b658b994p-1", "0x1.9a80155e16000p-2"),
    pair!("0x1.7f405ffc61022p-1", "0x1.abe186ed3d000p-2"),
    pair!("0x1.7ad22181415cap-1", "0x1.bd0f2aea0e000p-2"),
    pair!("0x1.767dcf99eff8cp-1", "0x1.ce0a43dbf4000p-2"),
];

/// `tab2`: c, and the part of c its `double` misses.
static TABLE2: [(f64, f64); N as usize] = [
    pair!("0x1.6200012b90a8ep-1", "0x1.904ab0644b605p-55"),
    pair!("0x1.66000045734a6p-1", "0x1.1ff9bea62f7a9p-57"),
    pair!("0x1.69fffc325f2c5p-1", "0x1.27ecfcb3c90bap-55"),
    pair!("0x1.6e00038b95a04p-1", "0x1.8ff8856739326p-55"),
    pair!("0x1.71fffe09994e3p-1", "0x1.afd40275f82b1p-55"),
    pair!("0x1.7600015590e1p-1", "-0x1.2fd75b4238341p-56"),
    pair!("0x1.7a00012655bd5p-1", "0x1.808e67c242b76p-56"),
    pair!("0x1.7e0003259e9a6p-1", "-0x1.208e426f622b7p-57"),
    pair!("0x1.81fffedb4b2d2p-1", "-0x1.402461ea5c92fp-55"),
    pair!("0x1.860002dfafcc3p-1", "0x1.df7f4a2f29a1fp-57"),
    pair!("0x1.89ffff78c6b5p-1", "-0x1.e0453094995fdp-55"),
    pair!("0x1.8e00039671566p-1", "-0x1.a04f3bec77b45p-55"),
    pair!("0x1.91fffe2bf1745p-1", "-0x1.7fa34400e203cp-56"),
    pair!("0x1.95fffcc5c9fd1p-1", "-0x1.6ff8005a0695dp-56"),
    pair!("0x1.9a0003bba4767p-1", "0x1.0f8c4c4ec7e03p-56"),
    pair!("0x1.9dfffe7b92da5p-1", "0x1.e7fd9478c4602p-55"),
    pair!("0x1.a1fffd72efdafp-1", "-0x1.a0c554dcdae7ep-57"),
    pair!("0x1.a5fffde04ff95p-1", "0x1.67da98ce9b26bp-55"),
    pair!("0x1.a9fffca5e8d2bp-1", "-0x1.284c9b54c13dep-55"),
    pair!("0x1.adfffddad03eap-1", "0x1.812c8ea602e3cp-58"),
    pair!("0x1.b1ffff10d3d4dp-1", "-0x1.efaddad27789cp-55"),
    pair!("0x1.b5fffce21165ap-1", "0x1.3cb1719c61237p-58"),
    pair!("0x1.b9fffd950e674p-1", "0x1.3f7d94194cep-56"),
    pair!("0x1.be000139ca8afp-1", "0x1.50ac4215d9bcp-56"),
    pair!("0x1.c20005b46df99p-1", "0x1.beea653e9c1c9p-57"),
    pair!("0x1.c600040b9f7aep-1", "-0x1.c079f274a70d6p-56"),
    pair!("0x1.ca0006255fd8ap-1", "-0x1.a0b4076e84c1fp-56"),
    pair!("0x1.cdfffd94c095dp-1", "0x1.8f933f99ab5d7p-55"),
    pair!("0x1.d1ffff975d6cfp-1", "-0x1.82c08665fe1bep-58"),
    pair!("0x1.d5fffa2561c93p-1", "-0x1.b04289bd295f3p-56"),
    pair!("0x1.d9fff9d228b0cp-1", "0x1.70251340fa236p-55"),
    pair!("0x1.de00065bc7e16p-1", "-0x1.5011e16a4d80cp-56"),
    pair!("0x1.e200002f64791p-1", "0x1.9802f09ef62ep-55"),
    pair!("0x1.e600057d7a6d8p-1", "-0x1.e0b75580cf7fap-56"),
    pair!("0x1.ea00027edc00cp-1", "-0x1.c848309459811p-55"),
    pair!("0x1.ee0006cf5cb7cp-1", "-0x1.f8027951576f4p-55"),
    pair!("0x1.f2000782b7dccp-1", "-0x1.f81d97274538fp-55"),
    pair!("0x1.f6000260c450ap-1", "-0x1.071002727ffdcp-59"),
    pair!("0x1.f9fffe88cd533p-1", "-0x1.81bdce1fda8bp-58"),
    pair!("0x1.fdfffd50f8689p-1", "0x1.7f91acb918e6ep-55"),
    pair!("0x1.0200004292367p+0", "0x1.b7ff365324681p-54"),
    pair!("0x1.05fffe3e3d668p+0", "0x1.6fa08ddae957bp-55"),
    pair!("0x1.0a0000a85a757p+0", "-0x1.7e2de80d3fb91p-58"),
    pair!("0x1.0e0001a5f3fccp+0", "-0x1.1823305c5f014p-54"),
    pair!("0x1.11ffff8afbaf5p+0", "-0x1.bfabb6680bac2p-55"),
    pair!("0x1.15fffe54d91adp+0", "-0x1.d7f121737e7efp-54"),
    pair!("0x1.1a00011ac36e1p+0", "0x1.c000a0516f5ffp-54"),
    pair!("0x1.1e00019c84248p+0", "-0x1.082fbe4da5dap-54"),
    pair!("0x1.220000ffe5e6ep+0", "-0x1.8fdd04c9cfb43p-55"),
    pair!("0x1.26000269fd891p+0", "0x1.cfe2a7994d182p-55"),
    pair!("0x1.2a00029a6e6dap+0", "-0x1.00273715e8bc5p-56"),
    pair!("0x1.2dfffe0293e39p+0", "0x1.b7c39dab2a6f9p-54"),
    pair!("0x1.31ffff7dcf082p+0", "0x1.df1336edc5254p-56"),
    pair!("0x1.35ffff05a8b6p+0", "-0x1.e03564ccd31ebp-54"),
    pair!("0x1.3a0002e0eaeccp+0", "0x1.5f0e74bd3a477p-56"),
    pair!("0x1.3e000043bb236p+0", "0x1.c7dcb149d8833p-54"),
    pair!("0x1.4200002d187ffp+0", "0x1.e08afcf2d3d28p-56"),
    pair!("0x1.460000d387cb1p+0", "0x1.20837856599a6p-55"),
    pair!("0x1.4a00004569f89p+0", "-0x1.9fa5c904fbcd2p-55"),
    pair!("0x1.4e000043543f3p+0", "-0x1.81125ed175329p-56"),
    pair!("0x1.51fffcc027f0fp+0", "0x1.883d8847754dcp-54"),
    pair!("0x1.55ffffd87b36fp+0", "-0x1.709e731d02807p-55"),
    pair!("0x1.59ffff21df7bap+0", "0x1.7f79f68727b02p-55"),
    pair!("0x1.5dfffebfc3481p+0", "-0x1.180902e30e93ep-54"),
];

/// The base-2 logarithm of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn log2(x: f64) -> f64 {
    let mut ix = x.to_bits();
    let top = top16(x);

    let low = (1.0 - hexf64!("0x1.5b51p-5")).to_bits();
    let high = (1.0 + hexf64!("0x1.6ab2p-5")).to_bits();
    if ix.wrapping_sub(low) < high - low {
        // x close to 1 is handled on its own.
        // Get the sign of zero right when rounding downward with x == 1.
        if ix == 1f64.to_bits() {
            return 0.0;
        }
        let r = x - 1.0;
        let rhi = f64::from_bits(r.to_bits() & (u64::MAX << 32));
        let rlo = r - rhi;
        let hi = rhi * INV_LN2_HI;
        let mut lo = rlo * INV_LN2_HI + r * INV_LN2_LO;
        // The rounding error of r² is 2^-62.
        let r2 = r * r;
        let r4 = r2 * r2;
        // The worst-case error is less than 0.55 ulp without fma.
        let p = r2 * (B0 + r * B1);
        let mut y = hi + p;
        lo += hi - y + p;
        lo += r4 * (B2 + r * B3 + r2 * (B4 + r * B5) + r4 * (B6 + r * B7 + r2 * (B8 + r * B9)));
        y += lo;
        return y;
    }
    if top.wrapping_sub(0x0010) >= 0x7ff0 - 0x0010 {
        // x < 2^-1022, or infinite, or NaN.
        if ix << 1 == 0 {
            return divzero(1);
        }
        if ix == f64::INFINITY.to_bits() {
            return x;
        }
        if top & 0x8000 != 0 || top & 0x7ff0 == 0x7ff0 {
            return invalid(x);
        }
        // x is subnormal: normalise it.
        ix = (x * hexf64!("0x1p52")).to_bits();
        ix = ix.wrapping_sub(52 << 52);
    }

    // x = 2^k·z, where z is in [OFF, 2·OFF) and exact. z's range is split into
    // N subintervals, and c is near the middle of z's.
    let tmp = ix.wrapping_sub(OFF);
    let i = (tmp >> (52 - TABLE_BITS)) % N;
    // An arithmetic shift.
    let k = (tmp.cast_signed() >> 52) as i32;
    let iz = ix.wrapping_sub(tmp & (0xfff << 52));
    let index = usize::try_from(i).unwrap_or(0);
    let (invc, logc) = TABLE.get(index).copied().unwrap_or((1.0, 0.0));
    let (chi, clo) = TABLE2.get(index).copied().unwrap_or((1.0, 0.0));
    let z = f64::from_bits(iz);
    let kd = f64::from(k);

    // log2(x) = log2(z/c) + log2(c) + k, with r ~= z/c - 1 and |r| < 1/2N.
    // Without fma, the rounding error is 2^-55/N + 2^-65.
    let r = (z - chi - clo) * invc;
    let rhi = f64::from_bits(r.to_bits() & (u64::MAX << 32));
    let rlo = r - rhi;
    let t1 = rhi * INV_LN2_HI;
    let t2 = rlo * INV_LN2_HI + r * INV_LN2_LO;

    // hi + lo = r/ln2 + log2(c) + k.
    let t3 = kd + logc;
    let hi = t3 + t1;
    let lo = t3 - hi + t1 + t2;

    // log2(r + 1) = r/ln2 + r²·poly(r). The rounding error of r² is 2^-54/N².
    let r2 = r * r;
    let r4 = r2 * r2;
    // The worst-case error if |y| > 2^-4 is 0.550 ulp without fma.
    let p = A0 + r * A1 + r2 * (A2 + r * A3) + r4 * (A4 + r * A5);
    lo + r2 * p + hi
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn log2_matches_libc_test() {
        let files = ["crlibm/log2.h", "sanity/log2.h", "special/log2.h"];
        mtest::d_d("log2", &files, |x| log2(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_tables_start_and_end_as_musls() {
        assert_eq!(
            TABLE.first(),
            Some(&pair!("0x1.724286bb1acf8p+0", "-0x1.1095feecdb000p-1"))
        );
        assert_eq!(
            TABLE.last(),
            Some(&pair!("0x1.767dcf99eff8cp-1", "0x1.ce0a43dbf4000p-2"))
        );
        assert_eq!(
            TABLE2.first(),
            Some(&pair!("0x1.6200012b90a8ep-1", "0x1.904ab0644b605p-55"))
        );
        assert_eq!(
            TABLE2.last(),
            Some(&pair!("0x1.5dfffebfc3481p+0", "-0x1.180902e30e93ep-54"))
        );
    }
}
