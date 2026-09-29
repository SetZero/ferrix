//! The C programs that exercise `regex.h`.
//!
//! Several cases are adapted from libc-test's regex tests, and say so in the
//! programs. The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn expressions_match_where_posix_says_and_report_their_subexpressions() {
    check(&Case::named("regex/regex"));
}

#[test]
fn a_pattern_that_would_make_a_backtracker_explode_finishes_at_once() {
    check(&Case::named("regex/regex_timing"));
}
