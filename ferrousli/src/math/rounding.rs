//! Rounding to integers: `ceil`, `floor`, `trunc`, `round`, `rint`,
//! `nearbyint`, and the conversions `lround`, `llround`, `lrint` and `llrint`.
//!
//! Every one is correctly rounded, and exact where C says so.
//!
//! Ported from musl 1.2.5's `ceil.c`, `floor.c`, `trunc.c`, `round.c`,
//! `rint.c`, `nearbyint.c`, `lround.c`, `llround.c`, their `float` versions,
//! and `x86_64/lrint.c` and `x86_64/llrint.c` (MIT; see [`crate::math`] for
//! the notice).
//!
//! # Inexact
//!
//! `ceil`, `floor`, `trunc` and `round` raise inexact when they discard a
//! fraction, as musl does and as C allows. `rint` raises it and `nearbyint`
//! never does. The `double` versions raise it with `x + 2^52 - 2^52`, which
//! rounds away the fraction at run time in the current mode; LLVM keeps that
//! sum as written, because rustc never lets it reassociate floating-point
//! arithmetic.

use core::ffi::{c_long, c_longlong};

use crate::fenv::{FE_INEXACT, feclearexcept, fetestexcept};
use crate::math::arch;
use crate::math::support::{force_eval, force_evalf, hexf32, hexf64};

/// 2^52, `1/DBL_EPSILON`: every `double` at least this large is an integer.
const TOINT: f64 = hexf64!("0x1p52");

/// 2^23, `1/FLT_EPSILON` for rounding `float`s the same way.
const TOINTF: f32 = hexf32!("0x1p23");

/// A value large enough that adding it to a non-integer rounds, raising
/// inexact.
const HUGE: f64 = hexf64!("0x1p120");

/// The smallest integer not less than `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ceil(x: f64) -> f64 {
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
    // Special because of the other rounding modes.
    if e < 0x3ff {
        force_eval(y);
        return if bits >> 63 != 0 { -0.0 } else { 1.0 };
    }
    if y < 0.0 { x + y + 1.0 } else { x + y }
}

/// [`ceil`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ceilf(x: f32) -> f32 {
    let mut bits = x.to_bits();
    let e = (bits >> 23 & 0xff) as i32 - 0x7f;
    if e >= 23 {
        return x;
    }
    if e >= 0 {
        let m = 0x007f_ffff >> e;
        if bits & m == 0 {
            return x;
        }
        force_eval(f64::from(x) + HUGE);
        if bits >> 31 == 0 {
            bits += m;
        }
        bits &= !m;
    } else {
        force_eval(f64::from(x) + HUGE);
        if bits >> 31 != 0 {
            bits = (-0.0f32).to_bits();
        } else if bits << 1 != 0 {
            bits = 1f32.to_bits();
        }
    }
    f32::from_bits(bits)
}

/// The largest integer not greater than `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn floor(x: f64) -> f64 {
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
    // Special because of the other rounding modes.
    if e < 0x3ff {
        force_eval(y);
        return if bits >> 63 != 0 { -1.0 } else { 0.0 };
    }
    if y > 0.0 { x + y - 1.0 } else { x + y }
}

/// [`floor`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn floorf(x: f32) -> f32 {
    let mut bits = x.to_bits();
    let e = (bits >> 23 & 0xff) as i32 - 0x7f;
    if e >= 23 {
        return x;
    }
    if e >= 0 {
        let m = 0x007f_ffff >> e;
        if bits & m == 0 {
            return x;
        }
        force_eval(f64::from(x) + HUGE);
        if bits >> 31 != 0 {
            bits += m;
        }
        bits &= !m;
    } else {
        force_eval(f64::from(x) + HUGE);
        if bits >> 31 == 0 {
            bits = 0;
        } else if bits << 1 != 0 {
            bits = (-1f32).to_bits();
        }
    }
    f32::from_bits(bits)
}

/// `x` with its fraction discarded.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn trunc(x: f64) -> f64 {
    let bits = x.to_bits();
    let mut e = (bits >> 52 & 0x7ff) as i32 - 0x3ff + 12;
    if e >= 52 + 12 {
        return x;
    }
    if e < 12 {
        e = 1;
    }
    let m = u64::MAX >> e;
    if bits & m == 0 {
        return x;
    }
    force_eval(x + HUGE);
    f64::from_bits(bits & !m)
}

/// [`trunc`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn truncf(x: f32) -> f32 {
    let bits = x.to_bits();
    let mut e = (bits >> 23 & 0xff) as i32 - 0x7f + 9;
    if e >= 23 + 9 {
        return x;
    }
    if e < 9 {
        e = 1;
    }
    let m = u32::MAX >> e;
    if bits & m == 0 {
        return x;
    }
    force_eval(f64::from(x) + HUGE);
    f32::from_bits(bits & !m)
}

/// `x` rounded to the nearest integer, ties away from zero, whatever the
/// rounding mode.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn round(x: f64) -> f64 {
    let bits = x.to_bits();
    let e = (bits >> 52 & 0x7ff) as i32;
    if e >= 0x3ff + 52 {
        return x;
    }
    let negative = bits >> 63 != 0;
    let a = if negative { -x } else { x };
    if e < 0x3ff - 1 {
        // Raises inexact if x is not zero.
        force_eval(a + TOINT);
        return 0.0 * x;
    }
    let mut y = a + TOINT - TOINT - a;
    y = if y > 0.5 {
        y + a - 1.0
    } else if y <= -0.5 {
        y + a + 1.0
    } else {
        y + a
    };
    if negative { -y } else { y }
}

/// [`round`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn roundf(x: f32) -> f32 {
    let bits = x.to_bits();
    let e = (bits >> 23 & 0xff) as i32;
    if e >= 0x7f + 23 {
        return x;
    }
    let negative = bits >> 31 != 0;
    let a = if negative { -x } else { x };
    if e < 0x7f - 1 {
        force_evalf(a + TOINTF);
        return 0.0 * x;
    }
    let mut y = a + TOINTF - TOINTF - a;
    y = if y > 0.5 {
        y + a - 1.0
    } else if y <= -0.5 {
        y + a + 1.0
    } else {
        y + a
    };
    if negative { -y } else { y }
}

/// [`round`], converted to a `long`. Out of range, or for a NaN, the result is
/// `LONG_MIN` and invalid is raised.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lround(x: f64) -> c_long {
    arch::trunc_to_i64(round(x))
}

/// [`lround`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lroundf(x: f32) -> c_long {
    arch::trunc_to_i64f(roundf(x))
}

/// [`round`], converted to a `long long`, as [`lround`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn llround(x: f64) -> c_longlong {
    arch::trunc_to_i64(round(x))
}

/// [`llround`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn llroundf(x: f32) -> c_longlong {
    arch::trunc_to_i64f(roundf(x))
}

/// `x` rounded to an integer in the current rounding mode, raising inexact if
/// that changes it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn rint(x: f64) -> f64 {
    let bits = x.to_bits();
    let e = (bits >> 52 & 0x7ff) as i32;
    let negative = bits >> 63 != 0;
    if e >= 0x3ff + 52 {
        return x;
    }
    let y = if negative {
        x - TOINT + TOINT
    } else {
        x + TOINT - TOINT
    };
    if y == 0.0 {
        return if negative { -0.0 } else { 0.0 };
    }
    y
}

/// [`rint`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn rintf(x: f32) -> f32 {
    let bits = x.to_bits();
    let e = (bits >> 23 & 0xff) as i32;
    let negative = bits >> 31 != 0;
    if e >= 0x7f + 23 {
        return x;
    }
    let y = if negative {
        x - TOINTF + TOINTF
    } else {
        x + TOINTF - TOINTF
    };
    if y == 0.0 {
        return if negative { -0.0 } else { 0.0 };
    }
    y
}

/// [`rint`], converted to a `long` by `cvtsd2si`, which rounds in the current
/// mode. Out of range, or for a NaN, the result is `LONG_MIN` and only invalid
/// is raised.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lrint(x: f64) -> c_long {
    arch::round_to_i64(x)
}

/// [`lrint`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lrintf(x: f32) -> c_long {
    arch::round_to_i64f(x)
}

/// [`rint`], converted to a `long long`, as [`lrint`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn llrint(x: f64) -> c_longlong {
    arch::round_to_i64(x)
}

/// [`llrint`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn llrintf(x: f32) -> c_longlong {
    arch::round_to_i64f(x)
}

/// [`rint`] without the inexact exception: the flag is as it was before.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn nearbyint(x: f64) -> f64 {
    let inexact = fetestexcept(FE_INEXACT);
    let y = rint(x);
    if inexact == 0 {
        let _ = feclearexcept(FE_INEXACT);
    }
    y
}

/// [`nearbyint`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn nearbyintf(x: f32) -> f32 {
    let inexact = fetestexcept(FE_INEXACT);
    let y = rintf(x);
    if inexact == 0 {
        let _ = feclearexcept(FE_INEXACT);
    }
    y
}
