//! The C programs that exercise what a C++ runtime needs from the C library
//! beyond what C programs already use: `dl_iterate_phdr` and `dladdr`, which
//! libunwind finds unwind tables and names with; the message catalogues of
//! `std::messages`; and the `_l` number parsers of `std::num_get`.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn the_program_is_the_one_object_with_its_code_and_this_threads_tls() {
    check(&Case::named("cxxrt/link"));
}

#[test]
fn catalogues_are_found_by_path_and_along_nlspath_and_messages_looked_up() {
    check(&Case::named("cxxrt/catgets"));
}

#[test]
fn the_locale_parsers_parse_as_the_plain_ones() {
    check(&Case::named("cxxrt/strtod_l"));
}
