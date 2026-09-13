//! Remainders: `fmod`, `remainder`, `remquo`, and `remainder`'s old name
//! `drem`.
//!
//! All are exact: the remainder of two floating-point numbers always is. They
//! work on the integer significands, shifting and subtracting one bit of the
//! quotient at a time, so no step can round.
//!
//! Ported from musl 1.2.5's `fmod.c`, `fmodf.c`, `remquo.c`, `remquof.c`,
//! `remainder.c` and `remainderf.c` (MIT; see [`crate::math`] for the notice).

use core::ffi::c_int;

/// `x - n*y` for the integer `n` that is `x/y` truncated, with `x`'s sign.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fmod(x: f64, y: f64) -> f64 {
    let ux = x.to_bits();
    let mut uy = y.to_bits();
    let mut ex = (ux >> 52 & 0x7ff) as i32;
    let mut ey = (uy >> 52 & 0x7ff) as i32;
    let sx = ux >> 63;
    let mut uxi = ux;

    if uy << 1 == 0 || y.is_nan() || ex == 0x7ff {
        return (x * y) / (x * y);
    }
    if uxi << 1 <= uy << 1 {
        if uxi << 1 == uy << 1 {
            return 0.0 * x;
        }
        return x;
    }

    // Normalise x and y.
    if ex == 0 {
        let mut i = uxi << 12;
        while i >> 63 == 0 {
            ex -= 1;
            i <<= 1;
        }
        uxi <<= 1 - ex;
    } else {
        uxi &= u64::MAX >> 12;
        uxi |= 1 << 52;
    }
    if ey == 0 {
        let mut i = uy << 12;
        while i >> 63 == 0 {
            ey -= 1;
            i <<= 1;
        }
        uy <<= 1 - ey;
    } else {
        uy &= u64::MAX >> 12;
        uy |= 1 << 52;
    }

    // x mod y
    while ex > ey {
        let i = uxi.wrapping_sub(uy);
        if i >> 63 == 0 {
            if i == 0 {
                return 0.0 * x;
            }
            uxi = i;
        }
        uxi <<= 1;
        ex -= 1;
    }
    let i = uxi.wrapping_sub(uy);
    if i >> 63 == 0 {
        if i == 0 {
            return 0.0 * x;
        }
        uxi = i;
    }
    while uxi >> 52 == 0 {
        uxi <<= 1;
        ex -= 1;
    }

    // Scale the result.
    if ex > 0 {
        uxi -= 1 << 52;
        uxi |= (ex as u64) << 52;
    } else {
        uxi >>= 1 - ex;
    }
    f64::from_bits(uxi | sx << 63)
}

/// [`fmod`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fmodf(x: f32, y: f32) -> f32 {
    let ux = x.to_bits();
    let mut uy = y.to_bits();
    let mut ex = (ux >> 23 & 0xff) as i32;
    let mut ey = (uy >> 23 & 0xff) as i32;
    let sx = ux & 0x8000_0000;
    let mut uxi = ux;

    if uy << 1 == 0 || y.is_nan() || ex == 0xff {
        return (x * y) / (x * y);
    }
    if uxi << 1 <= uy << 1 {
        if uxi << 1 == uy << 1 {
            return 0.0 * x;
        }
        return x;
    }

    // Normalise x and y.
    if ex == 0 {
        let mut i = uxi << 9;
        while i >> 31 == 0 {
            ex -= 1;
            i <<= 1;
        }
        uxi <<= 1 - ex;
    } else {
        uxi &= u32::MAX >> 9;
        uxi |= 1 << 23;
    }
    if ey == 0 {
        let mut i = uy << 9;
        while i >> 31 == 0 {
            ey -= 1;
            i <<= 1;
        }
        uy <<= 1 - ey;
    } else {
        uy &= u32::MAX >> 9;
        uy |= 1 << 23;
    }

    // x mod y
    while ex > ey {
        let i = uxi.wrapping_sub(uy);
        if i >> 31 == 0 {
            if i == 0 {
                return 0.0 * x;
            }
            uxi = i;
        }
        uxi <<= 1;
        ex -= 1;
    }
    let i = uxi.wrapping_sub(uy);
    if i >> 31 == 0 {
        if i == 0 {
            return 0.0 * x;
        }
        uxi = i;
    }
    while uxi >> 23 == 0 {
        uxi <<= 1;
        ex -= 1;
    }

    // Scale the result.
    if ex > 0 {
        uxi -= 1 << 23;
        uxi |= (ex as u32) << 23;
    } else {
        uxi >>= 1 - ex;
    }
    f32::from_bits(uxi | sx)
}

/// `x - n*y` for the integer `n` nearest `x/y`, ties to even, and the low 31
/// bits of `n` with its sign.
fn remquo_parts(x: f64, y: f64) -> (f64, c_int) {
    let ux = x.to_bits();
    let mut uy = y.to_bits();
    let mut ex = (ux >> 52 & 0x7ff) as i32;
    let mut ey = (uy >> 52 & 0x7ff) as i32;
    let sx = ux >> 63 != 0;
    let sy = uy >> 63 != 0;
    let mut uxi = ux;

    if uy << 1 == 0 || y.is_nan() || ex == 0x7ff {
        return ((x * y) / (x * y), 0);
    }
    if ux << 1 == 0 {
        return (x, 0);
    }

    // Normalise x and y.
    if ex == 0 {
        let mut i = uxi << 12;
        while i >> 63 == 0 {
            ex -= 1;
            i <<= 1;
        }
        uxi <<= 1 - ex;
    } else {
        uxi &= u64::MAX >> 12;
        uxi |= 1 << 52;
    }
    if ey == 0 {
        let mut i = uy << 12;
        while i >> 63 == 0 {
            ey -= 1;
            i <<= 1;
        }
        uy <<= 1 - ey;
    } else {
        uy &= u64::MAX >> 12;
        uy |= 1 << 52;
    }

    let mut q: u32 = 0;
    if ex < ey {
        if ex + 1 != ey {
            return (x, 0);
        }
        // |x| is between |y|/2 and |y|: the quotient is 0 or 1, decided below.
    } else {
        // x mod y
        while ex > ey {
            let i = uxi.wrapping_sub(uy);
            if i >> 63 == 0 {
                uxi = i;
                q += 1;
            }
            uxi <<= 1;
            q <<= 1;
            ex -= 1;
        }
        let i = uxi.wrapping_sub(uy);
        if i >> 63 == 0 {
            uxi = i;
            q += 1;
        }
        if uxi == 0 {
            ex = -60;
        } else {
            while uxi >> 52 == 0 {
                uxi <<= 1;
                ex -= 1;
            }
        }
    }

    // Scale the result, and decide between |x| and |x|-|y|.
    if ex > 0 {
        uxi -= 1 << 52;
        uxi |= (ex as u64) << 52;
    } else {
        uxi >>= 1 - ex;
    }
    let mut r = f64::from_bits(uxi);
    let ay = if sy { -y } else { y };
    if ex == ey || (ex + 1 == ey && (2.0 * r > ay || (2.0 * r == ay && q % 2 != 0))) {
        r -= ay;
        q = q.wrapping_add(1);
    }
    let q = (q & 0x7fff_ffff) as c_int;
    let quo = if sx != sy { -q } else { q };
    (if sx { -r } else { r }, quo)
}

/// [`remquo_parts`] for `float`.
fn remquof_parts(x: f32, y: f32) -> (f32, c_int) {
    let ux = x.to_bits();
    let mut uy = y.to_bits();
    let mut ex = (ux >> 23 & 0xff) as i32;
    let mut ey = (uy >> 23 & 0xff) as i32;
    let sx = ux >> 31 != 0;
    let sy = uy >> 31 != 0;
    let mut uxi = ux;

    if uy << 1 == 0 || y.is_nan() || ex == 0xff {
        return ((x * y) / (x * y), 0);
    }
    if ux << 1 == 0 {
        return (x, 0);
    }

    // Normalise x and y.
    if ex == 0 {
        let mut i = uxi << 9;
        while i >> 31 == 0 {
            ex -= 1;
            i <<= 1;
        }
        uxi <<= 1 - ex;
    } else {
        uxi &= u32::MAX >> 9;
        uxi |= 1 << 23;
    }
    if ey == 0 {
        let mut i = uy << 9;
        while i >> 31 == 0 {
            ey -= 1;
            i <<= 1;
        }
        uy <<= 1 - ey;
    } else {
        uy &= u32::MAX >> 9;
        uy |= 1 << 23;
    }

    let mut q: u32 = 0;
    if ex < ey {
        if ex + 1 != ey {
            return (x, 0);
        }
    } else {
        // x mod y
        while ex > ey {
            let i = uxi.wrapping_sub(uy);
            if i >> 31 == 0 {
                uxi = i;
                q += 1;
            }
            uxi <<= 1;
            q <<= 1;
            ex -= 1;
        }
        let i = uxi.wrapping_sub(uy);
        if i >> 31 == 0 {
            uxi = i;
            q += 1;
        }
        if uxi == 0 {
            ex = -30;
        } else {
            while uxi >> 23 == 0 {
                uxi <<= 1;
                ex -= 1;
            }
        }
    }

    // Scale the result, and decide between |x| and |x|-|y|.
    if ex > 0 {
        uxi -= 1 << 23;
        uxi |= (ex as u32) << 23;
    } else {
        uxi >>= 1 - ex;
    }
    let mut r = f32::from_bits(uxi);
    let ay = if sy { -y } else { y };
    if ex == ey || (ex + 1 == ey && (2.0 * r > ay || (2.0 * r == ay && q % 2 != 0))) {
        r -= ay;
        q = q.wrapping_add(1);
    }
    let q = (q & 0x7fff_ffff) as c_int;
    let quo = if sx != sy { -q } else { q };
    (if sx { -r } else { r }, quo)
}

/// [`remainder`], also storing in `*quo` the sign and at least the low three
/// bits of the quotient it rounded to.
///
/// # Safety
///
/// `quo` must be valid for writing an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn remquo(x: f64, y: f64, quo: *mut c_int) -> f64 {
    let (r, q) = remquo_parts(x, y);
    // SAFETY: the caller vouches for `quo`.
    unsafe { quo.write(q) };
    r
}

/// [`remquo`] for `float`.
///
/// # Safety
///
/// `quo` must be valid for writing an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn remquof(x: f32, y: f32, quo: *mut c_int) -> f32 {
    let (r, q) = remquof_parts(x, y);
    // SAFETY: the caller vouches for `quo`.
    unsafe { quo.write(q) };
    r
}

/// `x - n*y` for the integer `n` nearest `x/y`, ties to even.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn remainder(x: f64, y: f64) -> f64 {
    remquo_parts(x, y).0
}

/// [`remainder`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn remainderf(x: f32, y: f32) -> f32 {
    remquof_parts(x, y).0
}

/// [`remainder`] under its old BSD name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn drem(x: f64, y: f64) -> f64 {
    remquo_parts(x, y).0
}

/// [`remainderf`] under its old BSD name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dremf(x: f32, y: f32) -> f32 {
    remquof_parts(x, y).0
}
