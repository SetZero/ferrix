//! `math.h`: so far `sin`, `cos`, `exp`, `exp2`, `expm1`, `log`, `log2`,
//! `log10`, `log1p`, `pow` and `atan2` for `double`, `expf`, `exp2f`,
//! `expm1f`, `logf`, `log2f`, `log10f`, `log1pf` and `powf` for `float`, and
//! the classification functions the header's macros call for every type.
//!
//! # Where the code comes from
//!
//! Nearly all of it is ported from musl 1.2.5's `src/math`. musl took its
//! trigonometric functions from FreeBSD's msun, which carries Sun
//! Microsystems' notice, and its exponentials, logarithms and powers from
//! ARM's optimized-routines. Each module names the files it was ported from
//! and repeats the notices they carry, as their licences ask. The algorithms
//! and constants are theirs. Constants are written in C's hexadecimal notation
//! through [`support::hexf64`], or as the bits musl's comments give for its
//! decimal ones, so they can be compared with the source digit for digit.
//!
//! musl is MIT licensed:
//!
//! ```text
//! Copyright © 2005-2020 Rich Felker, et al.
//!
//! Permission is hereby granted, free of charge, to any person obtaining
//! a copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject to
//! the following conditions:
//!
//! The above copyright notice and this permission notice shall be
//! included in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//! EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
//! IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
//! CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
//! TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
//! SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//! ```
//!
//! # musl's bits
//!
//! Each function gives the result musl 1.2.5 gives on x86-64, bit for bit, and
//! raises the exceptions it raises, in every rounding mode. Both do `double`
//! arithmetic in SSE2 with `FLT_EVAL_METHOD` 0, and rustc never contracts a
//! multiplication and an addition into `fma`, so keeping musl's operations in
//! musl's order, parentheses and all, keeps its results.
//!
//! musl is compiled with `-frounding-math`, under which the compiler leaves a
//! constant expression whose result is inexact, such as `3*pio2_1t`, to be
//! evaluated at run time, rounded in the caller's mode. rustc would fold it to
//! nearest, so such an expression is written with [`support::barrier`] here.
//! A constant expression with an exact result is the same either way.
//!
//! LLVM also rewrites arithmetic in ways that are exact only when rounding to
//! nearest. It turns `a - b * C`, for a constant `C`, into `a + b * -C`, which
//! rounds the product the other way when rounding upward or downward. Where
//! musl subtracts such a product and its rounding matters, the product goes
//! through `barrier`. This was found, and the result checked, by building a
//! program against musl 1.2.5 and against this library, debug and release,
//! and comparing the bits and exceptions of every function for 800,000
//! arguments in each of the four rounding modes.
//! The exponentials and logarithms added after it, `exp2` to `powf`, were
//! compared the same way against a musl 1.2.5 build, for 200,000 arguments
//! each in each mode.
//!
//! # Errors
//!
//! As in musl, `math_errhandling` is `MATH_ERREXCEPT`: a domain error, a pole
//! or a range error raises the floating-point exception C names for it, and
//! `errno` is never set. glibc sets `errno` too; the headers here are musl's,
//! and say `MATH_ERREXCEPT`.
//!
//! # Code that must not call itself
//!
//! LLVM lowers some floating-point operations to calls into the C math
//! library, which in this library means calls into these very functions:
//!
//! * Rust's `%` on floats becomes a call to `fmod`.
//! * `powi`, `powf`, `sqrt`, `floor`, `mul_add` and their kin are std-only
//!   methods whose intrinsics become calls to `__powidf2`, `pow`, `sqrt`,
//!   `floor` and `fma` wherever the instruction set lacks an instruction.
//!
//! So nothing here uses them: everything is bit operations, conversions and
//! the four arithmetic operators.
//!
//! # Exceptions, and an optimiser that assumes there are none
//!
//! LLVM assumes the default floating-point environment: rounding to nearest,
//! and nobody reading the exception flags. So it may fold `1e300 * 1e300` to
//! infinity at compile time, or delete an operation whose result is unused,
//! and either loses the exception the operation was there to raise. musl's C
//! meets the same problem with `volatile`. Here:
//!
//! * [`support::barrier`] hides a value from the optimiser with
//!   [`core::hint::black_box`], which makes it go through memory, so an
//!   operation on it cannot be folded. musl calls this `fp_barrier`.
//! * [`support::force_eval`] hands a result to `black_box`, so an operation
//!   done only for its exceptions is kept. musl calls this `FORCE_EVAL`.
//!
//! An operation on constants alone folds before `black_box` sees the result,
//! so it is written `force_eval(barrier(0.0) / 0.0)`, never
//! `force_eval(0.0 / 0.0)`. An operation with a run-time operand, such as an
//! argument, is never folded. LLVM does not reassociate floating-point
//! arithmetic without fast-math flags, which rustc never sets, so musl's
//! `x + toint - toint` survives as written. The unit tests check the
//! exceptions of every table case in both the debug and the release build.

#[cfg(target_arch = "x86_64")]
#[path = "x86_64.rs"]
pub(crate) mod arch;
pub mod atan;
pub mod classify;
pub mod exp;
pub mod exp2;
pub mod expf;
pub mod expm1;
pub mod fma;
pub mod log;
pub mod log10;
pub mod log1p;
pub mod log2;
pub mod logf;
pub mod manipulate;
#[cfg(test)]
pub(crate) mod mtest;
pub mod pow;
pub mod powf;
pub mod remainder;
pub mod rounding;
pub mod sqrt;
pub(crate) mod support;
pub mod trig;
