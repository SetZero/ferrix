//! The C programs that exercise `math.h`: its functions called through the
//! header, and its classification and comparison macros on `float`, `double`
//! and `long double`.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn sin_cos_exp_log_pow_and_atan2_give_musls_bits() {
    check(&Case::named("math/functions"));
}

#[test]
fn classification_and_comparison_macros_on_every_type() {
    check(&Case::named("math/classify"));
}
