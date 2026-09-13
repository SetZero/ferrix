//! `math.h`: the elementary and special functions for `double` and `float`.
//!
//! `fenv.h`, which controls the rounding mode and exception flags these
//! functions honour and raise, is [`crate::fenv`].
//!
//! # Where the code comes from
//!
//! Nearly all of it is ported from musl 1.2.5's `src/math`. musl took most of
//! its `double` and `float` functions from FreeBSD's msun, which carries Sun
//! Microsystems' notice, and its exponentials, logarithms, powers and some
//! others from ARM's optimized-routines. Each module names the files it was
//! ported from and repeats the notices they carry, as their licences ask.
//! The algorithms and constants are theirs. Constants are written in C's
//! hexadecimal notation through [`support::hexf64`], so they can be compared
//! with the source digit for digit.
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
//! # Errors
//!
//! As in musl, `math_errhandling` is `MATH_ERREXCEPT`: a domain error, a pole
//! or a range error raises the floating-point exception C names for it, and
//! `errno` is never set.
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
//! So nothing here uses them. Square roots and conversions to integers are
//! SSE2 instructions, in [`arch`]; everything else is bit operations and the
//! four arithmetic operators.
//!
//! One of these functions calling another through its Rust path still calls
//! the exported symbol, since `no_mangle` names it. That is intended, and is
//! musl's structure too: `remainder` calls `remquo`.
//!
//! `tests/c_math.rs` checks the result on the built `libferrousli.a`. It
//! disassembles every object in the archive, builds the call graph from the
//! relocations, and fails if a function `math.h` declares is referenced but
//! not defined by the library's own objects, or can reach itself. Run it after
//! any change here.
//!
//! The archive also holds `compiler_builtins`, which defines weak `floor`,
//! `sqrt`, `fmod` and a few others for targets without a C math library. The
//! definitions here are strong, and the same test checks that a C program
//! linked against the archive gets them.
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

pub mod classify;
pub mod fma;
pub mod manipulate;
#[cfg(test)]
pub(crate) mod mtest;
pub mod remainder;
pub mod rounding;
pub mod sqrt;
pub(crate) mod support;
