//! The C program that exercises POSIX.1-2024's `endian.h` conversions.

mod common;

use common::{Case, check};

#[test]
fn endian_macros_and_callable_functions_convert_every_width() {
    check(&Case::named("endian"));
}
