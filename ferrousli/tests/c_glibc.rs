//! The C programs that call what glibc exports beyond the standard, the names
//! Chrome's headless shell and the Ubuntu libraries it loads link against:
//! glibc's checked functions, its internal and older names, the inline
//! stream macros its headers expand, and the GNU and Linux interfaces beside
//! them.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, Ending, SIGABRT, check};

#[test]
fn glibcs_checked_functions_do_the_plain_work_when_the_object_holds_it() {
    check(&Case::named("glibc/fortify"));
}

#[test]
fn a_checked_function_given_too_small_an_object_aborts_before_writing() {
    const OVERFLOW: &str = "*** buffer overflow detected ***: terminated\n";
    // What glibc 2.43 writes for each, read from the same program built
    // against it.
    const CASES: [(&[&str], &str); 9] = [
        (&["stpcpy"], OVERFLOW),
        (&["strncpy"], OVERFLOW),
        (&["strncat"], OVERFLOW),
        (
            &["fdelt"],
            "*** bit out of range 0 - FD_SETSIZE on fd_set ***: terminated\n",
        ),
        (&["read"], OVERFLOW),
        (&["poll"], OVERFLOW),
        (&["realpath"], OVERFLOW),
        (&["fgets"], OVERFLOW),
        (
            &["openat"],
            "*** invalid openat call: O_CREAT or O_TMPFILE without mode ***: terminated\n",
        ),
    ];
    for (args, stderr) in CASES {
        check(&Case {
            args,
            stderr,
            ending: Ending::Signal(SIGABRT),
            ..Case::named("glibc/fortify")
        });
    }
}

#[test]
fn glibcs_inline_stream_macros_reach_the_stream_through_uflow_and_overflow() {
    check(&Case::named("glibc/stdio"));
}

#[test]
fn glibcs_internal_and_older_names_are_the_standard_functions() {
    check(&Case::named("glibc/names"));
}
