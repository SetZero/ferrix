//! The C programs that exercise `stdlib.h`'s conversions, sorting, searching,
//! integer arithmetic and random numbers.
//!
//! Most are adapted from libc-test (MIT); each says where from. The harness is
//! in `tests/common`.

mod common;

use common::{Case, Ending, check};

#[test]
fn strtol_limits_bases_and_end_pointers() {
    check(&Case::named("stdlib/strtol"));
}

#[test]
fn integer_parsing_c23_prefixes_inttypes_and_atoi() {
    check(&Case::named("stdlib/strtol_more"));
}

#[test]
fn strtod_rounds_hard_cases_correctly() {
    check(&Case::named("stdlib/strtod"));
}

#[test]
fn strtod_round_trips_printed_doubles() {
    check(&Case::named("stdlib/strtod_simple"));
}

#[test]
fn strtod_rounds_forty_thousand_digits() {
    check(&Case::named("stdlib/strtod_long"));
}

#[test]
fn strtod_end_pointers_and_errno() {
    check(&Case::named("stdlib/strtod_more"));
}

#[test]
fn strtof_rounds_hard_cases_correctly() {
    check(&Case::named("stdlib/strtof"));
}

#[test]
fn strtold_rounds_hard_cases_correctly() {
    check(&Case::named("stdlib/strtold"));
}

#[test]
fn strtold_returns_in_st0_and_balances_the_x87_stack() {
    check(&Case::named("stdlib/long_double_abi"));
}

#[test]
fn abs_and_div_return_their_structures_by_value() {
    check(&Case::named("stdlib/div"));
}

#[test]
fn qsort_sorts_libc_tests_arrays() {
    check(&Case::named("stdlib/qsort"));
}

#[test]
fn qsort_r_large_elements_and_bsearch() {
    check(&Case::named("stdlib/qsort_r"));
}

#[test]
fn random_initstate_and_setstate() {
    check(&Case::named("stdlib/random"));
}

#[test]
fn lrand48_starts_deterministically() {
    check(&Case::named("stdlib/lrand48_signextend"));
}

#[test]
fn rand48_rand_and_rand_r_follow_their_specifications() {
    check(&Case::named("stdlib/rand48"));
}

#[test]
fn temporary_files_and_directories_get_exclusive_names() {
    check(&Case::named("stdlib/temp"));
}

#[test]
fn realpath_resolves_links_dots_and_the_working_directory() {
    check(&Case::named("stdlib/realpath"));
}

#[test]
fn posix_2024_conversions_suboptions_secure_environment_and_des_work() {
    check(&Case::named("stdlib/posix2024"));
}

#[test]
fn quick_exit_runs_its_separate_handler_stack_without_flushing() {
    check(&Case {
        stdout: "second\nfirst\n",
        ending: Ending::Code(23),
        ..Case::named("stdlib/quick_exit")
    });
}
