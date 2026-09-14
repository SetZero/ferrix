//! `pow`.
//!
//! Ported from musl 1.2.5's `pow.c`, `pow_data.c` and `pow_data.h` (MIT; see
//! [`crate::math`] for the notice), with the exponential's table from
//! [`crate::math::exp`]. musl took them from ARM's optimized-routines, whose
//! files carry this notice:
//!
//! ```text
//! Copyright (c) 2018, Arm Limited.
//! SPDX-License-Identifier: MIT
//! ```
//!
//! The table was converted from `pow_data.c` by a script, digit for digit.
//! musl's x86-64 build has no `__FP_FAST_FMA`, no `TOINT_INTRINSICS`, and
//! neither `WANT_SNAN` nor `WANT_ERRNO`, so the code kept is the code without
//! them. The worst-case error is 0.54 ulp.

use crate::math::exp::{
    C2, C3, C4, C5, INV_LN2_N, N, NEG_LN2_HI_N, NEG_LN2_LO_N, SHIFT, TABLE_BITS, table, top12,
};
use crate::math::support::{barrier, force_eval, hexf64, invalid, oflow, uflow};

/// `POW_LOG_TABLE_BITS`: z's range is split into 2^7 subintervals.
const LOG_TABLE_BITS: u32 = 7;
/// The number of subintervals.
const LOG_N: u64 = 1 << LOG_TABLE_BITS;
/// The bottom of z's range, [`OFF`, 2·`OFF`).
const OFF: u64 = 0x3fe6_9555_0000_0000;

/// The high bits of ln2.
const LN2HI: f64 = hexf64!("0x1.62e42fefa3800p-1");
/// The rest of ln2.
const LN2LO: f64 = hexf64!("0x1.ef35793c76730p-45");

// `poly`: log1p(r)'s coefficients after r, scaled as the evaluation scales
// them. A0 is -0.5.
const A0: f64 = hexf64!("-0x1p-1");
const A1: f64 = hexf64!("0x1.555555555556p-2") * -2.0;
const A2: f64 = hexf64!("-0x1.0000000000006p-2") * -2.0;
const A3: f64 = hexf64!("0x1.999999959554ep-3") * 4.0;
const A4: f64 = hexf64!("-0x1.555555529a47ap-3") * 4.0;
const A5: f64 = hexf64!("0x1.2495b9b4845e9p-3") * -8.0;
const A6: f64 = hexf64!("-0x1.0002b8b263fc3p-3") * -8.0;

/// A table row, written as musl's `A(invc, logc, logctail)` writes it.
macro_rules! a {
    ($invc:literal, $logc:literal, $logctail:literal) => {
        (hexf64!($invc), hexf64!($logc), hexf64!($logctail))
    };
}

/// `tab`: 1/c, log(c) rounded to 2^-43, and the rest of log(c), for the c
/// chosen in each subinterval. musl's unused padding field is left out.
static LOG_TABLE: [(f64, f64, f64); LOG_N as usize] = [
    a!("0x1.6a00000000000p+0", "-0x1.62c82f2b9c800p-2", "0x1.ab42428375680p-48"),
    a!("0x1.6800000000000p+0", "-0x1.5d1bdbf580800p-2", "-0x1.ca508d8e0f720p-46"),
    a!("0x1.6600000000000p+0", "-0x1.5767717455800p-2", "-0x1.362a4d5b6506dp-45"),
    a!("0x1.6400000000000p+0", "-0x1.51aad872df800p-2", "-0x1.684e49eb067d5p-49"),
    a!("0x1.6200000000000p+0", "-0x1.4be5f95777800p-2", "-0x1.41b6993293ee0p-47"),
    a!("0x1.6000000000000p+0", "-0x1.4618bc21c6000p-2", "0x1.3d82f484c84ccp-46"),
    a!("0x1.5e00000000000p+0", "-0x1.404308686a800p-2", "0x1.c42f3ed820b3ap-50"),
    a!("0x1.5c00000000000p+0", "-0x1.3a64c55694800p-2", "0x1.0b1c686519460p-45"),
    a!("0x1.5a00000000000p+0", "-0x1.347dd9a988000p-2", "0x1.5594dd4c58092p-45"),
    a!("0x1.5800000000000p+0", "-0x1.2e8e2bae12000p-2", "0x1.67b1e99b72bd8p-45"),
    a!("0x1.5600000000000p+0", "-0x1.2895a13de8800p-2", "0x1.5ca14b6cfb03fp-46"),
    a!("0x1.5600000000000p+0", "-0x1.2895a13de8800p-2", "0x1.5ca14b6cfb03fp-46"),
    a!("0x1.5400000000000p+0", "-0x1.22941fbcf7800p-2", "-0x1.65a242853da76p-46"),
    a!("0x1.5200000000000p+0", "-0x1.1c898c1699800p-2", "-0x1.fafbc68e75404p-46"),
    a!("0x1.5000000000000p+0", "-0x1.1675cababa800p-2", "0x1.f1fc63382a8f0p-46"),
    a!("0x1.4e00000000000p+0", "-0x1.1058bf9ae4800p-2", "-0x1.6a8c4fd055a66p-45"),
    a!("0x1.4c00000000000p+0", "-0x1.0a324e2739000p-2", "-0x1.c6bee7ef4030ep-47"),
    a!("0x1.4a00000000000p+0", "-0x1.0402594b4d000p-2", "-0x1.036b89ef42d7fp-48"),
    a!("0x1.4a00000000000p+0", "-0x1.0402594b4d000p-2", "-0x1.036b89ef42d7fp-48"),
    a!("0x1.4800000000000p+0", "-0x1.fb9186d5e4000p-3", "0x1.d572aab993c87p-47"),
    a!("0x1.4600000000000p+0", "-0x1.ef0adcbdc6000p-3", "0x1.b26b79c86af24p-45"),
    a!("0x1.4400000000000p+0", "-0x1.e27076e2af000p-3", "-0x1.72f4f543fff10p-46"),
    a!("0x1.4200000000000p+0", "-0x1.d5c216b4fc000p-3", "0x1.1ba91bbca681bp-45"),
    a!("0x1.4000000000000p+0", "-0x1.c8ff7c79aa000p-3", "0x1.7794f689f8434p-45"),
    a!("0x1.4000000000000p+0", "-0x1.c8ff7c79aa000p-3", "0x1.7794f689f8434p-45"),
    a!("0x1.3e00000000000p+0", "-0x1.bc286742d9000p-3", "0x1.94eb0318bb78fp-46"),
    a!("0x1.3c00000000000p+0", "-0x1.af3c94e80c000p-3", "0x1.a4e633fcd9066p-52"),
    a!("0x1.3a00000000000p+0", "-0x1.a23bc1fe2b000p-3", "-0x1.58c64dc46c1eap-45"),
    a!("0x1.3a00000000000p+0", "-0x1.a23bc1fe2b000p-3", "-0x1.58c64dc46c1eap-45"),
    a!("0x1.3800000000000p+0", "-0x1.9525a9cf45000p-3", "-0x1.ad1d904c1d4e3p-45"),
    a!("0x1.3600000000000p+0", "-0x1.87fa06520d000p-3", "0x1.bbdbf7fdbfa09p-45"),
    a!("0x1.3400000000000p+0", "-0x1.7ab890210e000p-3", "0x1.bdb9072534a58p-45"),
    a!("0x1.3400000000000p+0", "-0x1.7ab890210e000p-3", "0x1.bdb9072534a58p-45"),
    a!("0x1.3200000000000p+0", "-0x1.6d60fe719d000p-3", "-0x1.0e46aa3b2e266p-46"),
    a!("0x1.3000000000000p+0", "-0x1.5ff3070a79000p-3", "-0x1.e9e439f105039p-46"),
    a!("0x1.3000000000000p+0", "-0x1.5ff3070a79000p-3", "-0x1.e9e439f105039p-46"),
    a!("0x1.2e00000000000p+0", "-0x1.526e5e3a1b000p-3", "-0x1.0de8b90075b8fp-45"),
    a!("0x1.2c00000000000p+0", "-0x1.44d2b6ccb8000p-3", "0x1.70cc16135783cp-46"),
    a!("0x1.2c00000000000p+0", "-0x1.44d2b6ccb8000p-3", "0x1.70cc16135783cp-46"),
    a!("0x1.2a00000000000p+0", "-0x1.371fc201e9000p-3", "0x1.178864d27543ap-48"),
    a!("0x1.2800000000000p+0", "-0x1.29552f81ff000p-3", "-0x1.48d301771c408p-45"),
    a!("0x1.2600000000000p+0", "-0x1.1b72ad52f6000p-3", "-0x1.e80a41811a396p-45"),
    a!("0x1.2600000000000p+0", "-0x1.1b72ad52f6000p-3", "-0x1.e80a41811a396p-45"),
    a!("0x1.2400000000000p+0", "-0x1.0d77e7cd09000p-3", "0x1.a699688e85bf4p-47"),
    a!("0x1.2400000000000p+0", "-0x1.0d77e7cd09000p-3", "0x1.a699688e85bf4p-47"),
    a!("0x1.2200000000000p+0", "-0x1.fec9131dbe000p-4", "-0x1.575545ca333f2p-45"),
    a!("0x1.2000000000000p+0", "-0x1.e27076e2b0000p-4", "0x1.a342c2af0003cp-45"),
    a!("0x1.2000000000000p+0", "-0x1.e27076e2b0000p-4", "0x1.a342c2af0003cp-45"),
    a!("0x1.1e00000000000p+0", "-0x1.c5e548f5bc000p-4", "-0x1.d0c57585fbe06p-46"),
    a!("0x1.1c00000000000p+0", "-0x1.a926d3a4ae000p-4", "0x1.53935e85baac8p-45"),
    a!("0x1.1c00000000000p+0", "-0x1.a926d3a4ae000p-4", "0x1.53935e85baac8p-45"),
    a!("0x1.1a00000000000p+0", "-0x1.8c345d631a000p-4", "0x1.37c294d2f5668p-46"),
    a!("0x1.1a00000000000p+0", "-0x1.8c345d631a000p-4", "0x1.37c294d2f5668p-46"),
    a!("0x1.1800000000000p+0", "-0x1.6f0d28ae56000p-4", "-0x1.69737c93373dap-45"),
    a!("0x1.1600000000000p+0", "-0x1.51b073f062000p-4", "0x1.f025b61c65e57p-46"),
    a!("0x1.1600000000000p+0", "-0x1.51b073f062000p-4", "0x1.f025b61c65e57p-46"),
    a!("0x1.1400000000000p+0", "-0x1.341d7961be000p-4", "0x1.c5edaccf913dfp-45"),
    a!("0x1.1400000000000p+0", "-0x1.341d7961be000p-4", "0x1.c5edaccf913dfp-45"),
    a!("0x1.1200000000000p+0", "-0x1.16536eea38000p-4", "0x1.47c5e768fa309p-46"),
    a!("0x1.1000000000000p+0", "-0x1.f0a30c0118000p-5", "0x1.d599e83368e91p-45"),
    a!("0x1.1000000000000p+0", "-0x1.f0a30c0118000p-5", "0x1.d599e83368e91p-45"),
    a!("0x1.0e00000000000p+0", "-0x1.b42dd71198000p-5", "0x1.c827ae5d6704cp-46"),
    a!("0x1.0e00000000000p+0", "-0x1.b42dd71198000p-5", "0x1.c827ae5d6704cp-46"),
    a!("0x1.0c00000000000p+0", "-0x1.77458f632c000p-5", "-0x1.cfc4634f2a1eep-45"),
    a!("0x1.0c00000000000p+0", "-0x1.77458f632c000p-5", "-0x1.cfc4634f2a1eep-45"),
    a!("0x1.0a00000000000p+0", "-0x1.39e87b9fec000p-5", "0x1.502b7f526feaap-48"),
    a!("0x1.0a00000000000p+0", "-0x1.39e87b9fec000p-5", "0x1.502b7f526feaap-48"),
    a!("0x1.0800000000000p+0", "-0x1.f829b0e780000p-6", "-0x1.980267c7e09e4p-45"),
    a!("0x1.0800000000000p+0", "-0x1.f829b0e780000p-6", "-0x1.980267c7e09e4p-45"),
    a!("0x1.0600000000000p+0", "-0x1.7b91b07d58000p-6", "-0x1.88d5493faa639p-45"),
    a!("0x1.0400000000000p+0", "-0x1.fc0a8b0fc0000p-7", "-0x1.f1e7cf6d3a69cp-50"),
    a!("0x1.0400000000000p+0", "-0x1.fc0a8b0fc0000p-7", "-0x1.f1e7cf6d3a69cp-50"),
    a!("0x1.0200000000000p+0", "-0x1.fe02a6b100000p-8", "-0x1.9e23f0dda40e4p-46"),
    a!("0x1.0200000000000p+0", "-0x1.fe02a6b100000p-8", "-0x1.9e23f0dda40e4p-46"),
    a!("0x1.0000000000000p+0", "0x0.0000000000000p+0", "0x0.0000000000000p+0"),
    a!("0x1.0000000000000p+0", "0x0.0000000000000p+0", "0x0.0000000000000p+0"),
    a!("0x1.fc00000000000p-1", "0x1.0101575890000p-7", "-0x1.0c76b999d2be8p-46"),
    a!("0x1.f800000000000p-1", "0x1.0205658938000p-6", "-0x1.3dc5b06e2f7d2p-45"),
    a!("0x1.f400000000000p-1", "0x1.8492528c90000p-6", "-0x1.aa0ba325a0c34p-45"),
    a!("0x1.f000000000000p-1", "0x1.0415d89e74000p-5", "0x1.111c05cf1d753p-47"),
    a!("0x1.ec00000000000p-1", "0x1.466aed42e0000p-5", "-0x1.c167375bdfd28p-45"),
    a!("0x1.e800000000000p-1", "0x1.894aa149fc000p-5", "-0x1.97995d05a267dp-46"),
    a!("0x1.e400000000000p-1", "0x1.ccb73cdddc000p-5", "-0x1.a68f247d82807p-46"),
    a!("0x1.e200000000000p-1", "0x1.eea31c006c000p-5", "-0x1.e113e4fc93b7bp-47"),
    a!("0x1.de00000000000p-1", "0x1.1973bd1466000p-4", "-0x1.5325d560d9e9bp-45"),
    a!("0x1.da00000000000p-1", "0x1.3bdf5a7d1e000p-4", "0x1.cc85ea5db4ed7p-45"),
    a!("0x1.d600000000000p-1", "0x1.5e95a4d97a000p-4", "-0x1.c69063c5d1d1ep-45"),
    a!("0x1.d400000000000p-1", "0x1.700d30aeac000p-4", "0x1.c1e8da99ded32p-49"),
    a!("0x1.d000000000000p-1", "0x1.9335e5d594000p-4", "0x1.3115c3abd47dap-45"),
    a!("0x1.cc00000000000p-1", "0x1.b6ac88dad6000p-4", "-0x1.390802bf768e5p-46"),
    a!("0x1.ca00000000000p-1", "0x1.c885801bc4000p-4", "0x1.646d1c65aacd3p-45"),
    a!("0x1.c600000000000p-1", "0x1.ec739830a2000p-4", "-0x1.dc068afe645e0p-45"),
    a!("0x1.c400000000000p-1", "0x1.fe89139dbe000p-4", "-0x1.534d64fa10afdp-45"),
    a!("0x1.c000000000000p-1", "0x1.1178e8227e000p-3", "0x1.1ef78ce2d07f2p-45"),
    a!("0x1.be00000000000p-1", "0x1.1aa2b7e23f000p-3", "0x1.ca78e44389934p-45"),
    a!("0x1.ba00000000000p-1", "0x1.2d1610c868000p-3", "0x1.39d6ccb81b4a1p-47"),
    a!("0x1.b800000000000p-1", "0x1.365fcb0159000p-3", "0x1.62fa8234b7289p-51"),
    a!("0x1.b400000000000p-1", "0x1.4913d8333b000p-3", "0x1.5837954fdb678p-45"),
    a!("0x1.b200000000000p-1", "0x1.527e5e4a1b000p-3", "0x1.633e8e5697dc7p-45"),
    a!("0x1.ae00000000000p-1", "0x1.6574ebe8c1000p-3", "0x1.9cf8b2c3c2e78p-46"),
    a!("0x1.ac00000000000p-1", "0x1.6f0128b757000p-3", "-0x1.5118de59c21e1p-45"),
    a!("0x1.aa00000000000p-1", "0x1.7898d85445000p-3", "-0x1.c661070914305p-46"),
    a!("0x1.a600000000000p-1", "0x1.8beafeb390000p-3", "-0x1.73d54aae92cd1p-47"),
    a!("0x1.a400000000000p-1", "0x1.95a5adcf70000p-3", "0x1.7f22858a0ff6fp-47"),
    a!("0x1.a000000000000p-1", "0x1.a93ed3c8ae000p-3", "-0x1.8724350562169p-45"),
    a!("0x1.9e00000000000p-1", "0x1.b31d8575bd000p-3", "-0x1.c358d4eace1aap-47"),
    a!("0x1.9c00000000000p-1", "0x1.bd087383be000p-3", "-0x1.d4bc4595412b6p-45"),
    a!("0x1.9a00000000000p-1", "0x1.c6ffbc6f01000p-3", "-0x1.1ec72c5962bd2p-48"),
    a!("0x1.9600000000000p-1", "0x1.db13db0d49000p-3", "-0x1.aff2af715b035p-45"),
    a!("0x1.9400000000000p-1", "0x1.e530effe71000p-3", "0x1.212276041f430p-51"),
    a!("0x1.9200000000000p-1", "0x1.ef5ade4dd0000p-3", "-0x1.a211565bb8e11p-51"),
    a!("0x1.9000000000000p-1", "0x1.f991c6cb3b000p-3", "0x1.bcbecca0cdf30p-46"),
    a!("0x1.8c00000000000p-1", "0x1.07138604d5800p-2", "0x1.89cdb16ed4e91p-48"),
    a!("0x1.8a00000000000p-1", "0x1.0c42d67616000p-2", "0x1.7188b163ceae9p-45"),
    a!("0x1.8800000000000p-1", "0x1.1178e8227e800p-2", "-0x1.c210e63a5f01cp-45"),
    a!("0x1.8600000000000p-1", "0x1.16b5ccbacf800p-2", "0x1.b9acdf7a51681p-45"),
    a!("0x1.8400000000000p-1", "0x1.1bf99635a6800p-2", "0x1.ca6ed5147bdb7p-45"),
    a!("0x1.8200000000000p-1", "0x1.214456d0eb800p-2", "0x1.a87deba46baeap-47"),
    a!("0x1.7e00000000000p-1", "0x1.2bef07cdc9000p-2", "0x1.a9cfa4a5004f4p-45"),
    a!("0x1.7c00000000000p-1", "0x1.314f1e1d36000p-2", "-0x1.8e27ad3213cb8p-45"),
    a!("0x1.7a00000000000p-1", "0x1.36b6776be1000p-2", "0x1.16ecdb0f177c8p-46"),
    a!("0x1.7800000000000p-1", "0x1.3c25277333000p-2", "0x1.83b54b606bd5cp-46"),
    a!("0x1.7600000000000p-1", "0x1.419b423d5e800p-2", "0x1.8e436ec90e09dp-47"),
    a!("0x1.7400000000000p-1", "0x1.4718dc271c800p-2", "-0x1.f27ce0967d675p-45"),
    a!("0x1.7200000000000p-1", "0x1.4c9e09e173000p-2", "-0x1.e20891b0ad8a4p-45"),
    a!("0x1.7000000000000p-1", "0x1.522ae0738a000p-2", "0x1.ebe708164c759p-45"),
    a!("0x1.6e00000000000p-1", "0x1.57bf753c8d000p-2", "0x1.fadedee5d40efp-46"),
    a!("0x1.6c00000000000p-1", "0x1.5d5bddf596000p-2", "-0x1.a0b2a08a465dcp-47"),
];

/// Added to k to make the result negative.
const SIGN_BIAS: u32 = 0x800 << TABLE_BITS;

/// `x` without its sign, as musl's `fabs`.
const fn abs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & (u64::MAX >> 1))
}

/// log(x) as a rounded result and a tail with about 15 more bits, where `ix`
/// is x's bits, normalised in the subnormal range using the sign bit for the
/// exponent. Returns `(y, tail)`.
fn log_inline(ix: u64) -> (f64, f64) {
    // x = 2^k·z, where z is in [OFF, 2·OFF) and exact.
    let tmp = ix.wrapping_sub(OFF);
    let i = (tmp >> (52 - LOG_TABLE_BITS)) % LOG_N;
    // An arithmetic shift.
    let k = (tmp.cast_signed() >> 52) as i32;
    let iz = ix.wrapping_sub(tmp & (0xfff << 52));
    let z = f64::from_bits(iz);
    let kd = f64::from(k);

    // log(x) = k·ln2 + log(c) + log1p(z/c - 1).
    let (invc, logc, logctail) = usize::try_from(i)
        .ok()
        .and_then(|i| LOG_TABLE.get(i))
        .copied()
        .unwrap_or((1.0, 0.0, 0.0));

    // 1/c is j/N or j/N/2 for an integer j in [N, 2N), and |z/c - 1| < 1/N,
    // so r = z/c - 1 is exact. Split z so rhi, rlo and rhi·rhi are exact and
    // |rlo| <= |r|.
    let zhi = f64::from_bits(iz.wrapping_add(1 << 31) & (u64::MAX << 32));
    let zlo = z - zhi;
    let rhi = zhi * invc - 1.0;
    let rlo = zlo * invc;
    let r = rhi + rlo;

    // k·ln2 + log(c) + r.
    let t1 = kd * LN2HI + logc;
    let t2 = t1 + r;
    let lo1 = kd * LN2LO + logctail;
    let lo2 = t1 - t2 + r;

    // k·ln2 + log(c) + r + A0·r·r.
    let ar = A0 * r;
    let ar2 = r * ar;
    let ar3 = r * ar2;
    let arhi = A0 * rhi;
    let arhi2 = rhi * arhi;
    let hi = t2 + arhi2;
    let lo3 = rlo * (ar + arhi);
    let lo4 = t2 - hi + arhi2;
    // p = log1p(r) - r - A0·r·r.
    let p = ar3 * (A1 + r * A2 + ar2 * (A3 + r * A4 + ar2 * (A5 + r * A6)));
    let lo = lo1 + lo2 + lo3 + lo4 + p;
    let y = hi + lo;
    (y, hi - y + lo)
}

/// The cases that may overflow or underflow in computing scale·(1 + tmp)
/// without rounding in between. As `exp`'s, but scale may be negative.
fn specialcase(tmp: f64, sbits: u64, ki: u64) -> f64 {
    if ki & 0x8000_0000 == 0 {
        // k > 0: scale's exponent may have overflowed by up to 460.
        let scale = f64::from_bits(sbits.wrapping_sub(1009 << 52));
        return hexf64!("0x1p1009") * (scale + scale * tmp);
    }
    // k < 0: take care in the subnormal range.
    let sbits = sbits.wrapping_add(1022 << 52);
    // sbits is the signed scale.
    let scale = f64::from_bits(sbits);
    let mut y = scale + scale * tmp;
    if abs(y) < 1.0 {
        // Round y to its final precision before scaling it into the subnormal
        // range, so it is not rounded twice.
        let one = if y < 0.0 { -1.0 } else { 1.0 };
        let mut lo = scale - y + scale * tmp;
        let hi = one + y;
        lo = one - hi + y + lo;
        y = (hi + lo) - one;
        // Fix the sign of zero.
        if y == 0.0 {
            y = f64::from_bits(sbits & 0x8000_0000_0000_0000);
        }
        // The underflow exception must be raised explicitly.
        force_eval(barrier(hexf64!("0x1p-1022")) * hexf64!("0x1p-1022"));
    }
    hexf64!("0x1p-1022") * y
}

/// sign·exp(x + xtail), where |xtail| < 2^-8/N and |xtail| <= |x|, and
/// `sign_bias` is [`SIGN_BIAS`] for a negative sign or 0 for a positive one.
fn exp_inline(x: f64, xtail: f64, sign_bias: u32) -> f64 {
    let mut abstop = top12(x) & 0x7ff;
    if abstop.wrapping_sub(top12(hexf64!("0x1p-54")))
        >= top12(512.0).wrapping_sub(top12(hexf64!("0x1p-54")))
    {
        if abstop.wrapping_sub(top12(hexf64!("0x1p-54"))) >= 0x8000_0000 {
            // Tiny x, including 0, without a spurious underflow.
            let one = 1.0 + x;
            return if sign_bias != 0 { -one } else { one };
        }
        if abstop >= top12(1024.0) {
            // Infinities and NaNs were handled by the caller.
            return if x.to_bits() >> 63 != 0 {
                uflow(sign_bias)
            } else {
                oflow(sign_bias)
            };
        }
        // Large x is handled by `specialcase` below.
        abstop = 0;
    }

    // exp(x) = 2^(k/N)·exp(r), with exp(r) in [2^(-1/2N), 2^(1/2N)] and
    // x = ln2/N·k + r.
    let z = INV_LN2_N * x;
    // z - kd is in [-1, 1] in the rounding modes other than to nearest.
    let mut kd = z + SHIFT;
    let ki = kd.to_bits();
    kd -= SHIFT;
    let mut r = x + kd * NEG_LN2_HI_N + kd * NEG_LN2_LO_N;
    // The code assumes 2^-200 < |xtail| < 2^-8/N.
    r += xtail;
    // 2^(k/N) ~= scale·(1 + tail).
    let index = 2 * (ki % N);
    let top = ki.wrapping_add(u64::from(sign_bias)) << (52 - TABLE_BITS);
    let tail = f64::from_bits(table(index));
    // Only a valid scale when -1023·N < k < 1024·N.
    let sbits = table(index + 1).wrapping_add(top);
    // exp(x) ~= scale + scale·(tail + exp(r) - 1).
    let r2 = r * r;
    let tmp = tail + r + r2 * (C2 + r * C3) + r2 * r2 * (C4 + r * C5);
    if abstop == 0 {
        return specialcase(tmp, sbits, ki);
    }
    let scale = f64::from_bits(sbits);
    scale + scale * tmp
}

/// 0 if `iy`, the bits of a nonzero finite `double`, is not an integer, 1 if
/// it is an odd one, and 2 if an even one.
fn checkint(iy: u64) -> i32 {
    let e = (iy >> 52 & 0x7ff) as u32;
    if e < 0x3ff {
        return 0;
    }
    if e > 0x3ff + 52 {
        return 2;
    }
    let unit = 1u64 << (0x3ff + 52 - e);
    if iy & (unit - 1) != 0 {
        return 0;
    }
    if iy & unit != 0 {
        return 1;
    }
    2
}

/// Whether `i` is the bits of a zero, an infinity or a NaN.
const fn zeroinfnan(i: u64) -> bool {
    i.wrapping_mul(2).wrapping_sub(1) >= f64::INFINITY.to_bits().wrapping_mul(2) - 1
}

/// `x` raised to the power `y`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pow(x: f64, y: f64) -> f64 {
    let one = 1f64.to_bits();
    let infinity = f64::INFINITY.to_bits();
    let mut sign_bias = 0;
    let mut ix = x.to_bits();
    let iy = y.to_bits();
    let mut topx = top12(x);
    let topy = top12(y);
    if topx.wrapping_sub(0x001) >= 0x7ff - 0x001
        || (topy & 0x7ff).wrapping_sub(0x3be) >= 0x43e - 0x3be
    {
        // If |y| > 1075·ln2·2^53 ~= 0x1.749p62 then pow(x, y) is infinite or
        // 0, and if |y| < 2^-54/1075 ~= 0x1.e7b6p-65 it is ±1. So the special
        // cases are x < 2^-126, infinite or NaN, or |y| < 2^-65, |y| >= 2^63
        // or NaN.
        if zeroinfnan(iy) {
            if iy.wrapping_mul(2) == 0 {
                return 1.0;
            }
            if ix == one {
                return 1.0;
            }
            if ix.wrapping_mul(2) > infinity.wrapping_mul(2)
                || iy.wrapping_mul(2) > infinity.wrapping_mul(2)
            {
                return x + y;
            }
            if ix.wrapping_mul(2) == one.wrapping_mul(2) {
                return 1.0;
            }
            if (ix.wrapping_mul(2) < one.wrapping_mul(2)) == (iy >> 63 == 0) {
                // |x| < 1 and y is +inf, or |x| > 1 and y is -inf.
                return 0.0;
            }
            return y * y;
        }
        if zeroinfnan(ix) {
            let mut x2 = x * x;
            if ix >> 63 != 0 && checkint(iy) == 1 {
                x2 = -x2;
            }
            // Without the barrier, some compilers hoist 1/x2 out of the branch
            // and raise divide-by-zero spuriously.
            return if iy >> 63 != 0 {
                barrier(1.0 / x2)
            } else {
                x2
            };
        }
        // x and y are nonzero and finite.
        if ix >> 63 != 0 {
            // x < 0.
            let yint = checkint(iy);
            if yint == 0 {
                return invalid(x);
            }
            if yint == 1 {
                sign_bias = SIGN_BIAS;
            }
            ix &= 0x7fff_ffff_ffff_ffff;
            topx &= 0x7ff;
        }
        if (topy & 0x7ff).wrapping_sub(0x3be) >= 0x43e - 0x3be {
            // sign_bias is 0 here, because y is not odd.
            if ix == one {
                return 1.0;
            }
            if (topy & 0x7ff) < 0x3be {
                // |y| < 2^-65: x^y ~= 1 + y·log(x).
                return if ix > one { 1.0 + y } else { 1.0 - y };
            }
            return if (ix > one) == (topy < 0x800) {
                oflow(0)
            } else {
                uflow(0)
            };
        }
        if topx == 0 {
            // Normalise subnormal x so its exponent becomes negative.
            ix = (x * hexf64!("0x1p52")).to_bits();
            ix &= 0x7fff_ffff_ffff_ffff;
            ix = ix.wrapping_sub(52 << 52);
        }
    }

    let (hi, lo) = log_inline(ix);
    let yhi = f64::from_bits(iy & (u64::MAX << 27));
    let ylo = y - yhi;
    let lhi = f64::from_bits(hi.to_bits() & (u64::MAX << 27));
    let llo = hi - lhi + lo;
    let ehi = yhi * lhi;
    // |elo| < |ehi|·2^-25.
    let elo = ylo * lhi + y * llo;
    exp_inline(ehi, elo, sign_bias)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn pow_matches_libc_test() {
        let files = ["crlibm/pow.h", "ucb/pow.h", "sanity/pow.h", "special/pow.h"];
        // libc-test's `pow.c` tolerates wrong exceptions for a result below
        // the normal range that raised underflow.
        mtest::dd_d("pow", &files, |x, y| pow(x, y), Rules::ULP.underflow(), &[]);
    }

    #[test]
    fn integers_are_recognised() {
        assert_eq!(checkint(3f64.to_bits()), 1);
        assert_eq!(checkint(4f64.to_bits()), 2);
        assert_eq!(checkint(0.5f64.to_bits()), 0);
        assert_eq!(checkint(hexf64!("0x1p60").to_bits()), 2);
        assert!(zeroinfnan(0));
        assert!(zeroinfnan((-0.0f64).to_bits()));
        assert!(zeroinfnan(f64::NAN.to_bits()));
        assert!(!zeroinfnan(f64::MIN_POSITIVE.to_bits()));
    }
}
