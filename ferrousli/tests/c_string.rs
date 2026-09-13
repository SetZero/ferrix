//! The C programs that exercise `string.h`, `strings.h` and `ctype.h`.
//!
//! Each is built with `-fno-builtin`, so the compiler cannot answer a call on
//! constant arguments itself, and every check reaches the library. Several are
//! adapted from libc-test, and say so.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

/// Keeps the compiler from folding calls to the functions under test.
const NO_BUILTIN: &[&str] = &["-fno-builtin"];

/// A program in `tests/c/string`, built with [`NO_BUILTIN`].
const fn string(name: &'static str) -> Case {
    Case {
        cflags: NO_BUILTIN,
        ..Case::named(name)
    }
}

#[test]
fn copies_concatenation_spans_and_tokens() {
    check(&string("string/string"));
}

#[test]
fn the_rest_of_string_h_and_strings_h() {
    check(&string("string/misc"));
}

#[test]
fn memmem_finds_short_long_and_periodic_needles() {
    check(&string("string/memmem"));
}

#[test]
fn strstr_and_strcasestr() {
    check(&string("string/strstr"));
}

#[test]
fn strchr_at_every_alignment() {
    check(&string("string/strchr"));
}

#[test]
fn strcspn_strspn_and_strpbrk() {
    check(&string("string/strcspn"));
}

#[test]
fn the_word_at_a_time_functions_at_every_alignment_and_length() {
    check(&string("string/sweep"));
}

#[test]
fn memcpy_mempcpy_and_memmove_at_every_alignment() {
    check(&string("string/memcpy"));
}

#[test]
fn memset_and_bzero_at_every_alignment() {
    check(&string("string/memset"));
}

#[test]
fn ctype_functions_and_glibcs_tables() {
    check(&string("string/ctype"));
}

#[test]
fn strerror_strerror_r_and_strsignal() {
    check(&string("string/strerror"));
}

#[test]
fn strdup_and_strndup_copy_into_fresh_memory() {
    check(&string("string/dup"));
}
