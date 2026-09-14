//! The C programs that exercise the POSIX utilities built from logic over the
//! rest of the library: `dirent.h`, `getopt`, `fnmatch.h` and `libgen.h`.
//!
//! The libgen programs are adapted from libc-test, and say so. The getopt
//! programs' expected traces and error reports were generated once from the
//! host glibc.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn directories_are_read_across_buffers_positioned_and_scanned() {
    check(&Case::named("misc/dirent"));
}

#[test]
fn getopt_permutes_and_reports_errors_as_glibc_does() {
    check(&Case {
        stderr: "prog: invalid option -- 'z'\n\
                 prog: option requires an argument -- 'b'\n\
                 prog: invalid option -- ':'\n",
        ..Case::named("misc/getopt")
    });
}

#[test]
fn getopt_long_matches_abbreviates_and_reports_errors_as_glibc_does() {
    check(&Case {
        stderr: "prog: option '--col' is ambiguous; possibilities: '--color' '--column'\n\
                 prog: option '--co=1' is ambiguous; possibilities: '--color' '--column'\n\
                 prog: unrecognized option '--unknown=3'\n\
                 prog: option '--verbose' doesn't allow an argument\n\
                 prog: option '--column' requires an argument\n\
                 prog: option '-out' doesn't allow an argument\n\
                 prog: option '-col' is ambiguous; possibilities: '-color' '-colour' '-column'\n\
                 prog: unrecognized option '-z'\n\
                 prog: unrecognized option '-colors'\n\
                 prog: unrecognized option '-W bogus'\n\
                 prog: option requires an argument -- 'W'\n",
        ..Case::named("misc/getopt_long")
    });
}

#[test]
fn fnmatch_matches_wildcards_brackets_and_paths() {
    check(&Case::named("misc/fnmatch"));
}

#[test]
fn basename_takes_the_last_component() {
    check(&Case::named("misc/basename"));
}

#[test]
fn dirname_drops_the_last_component() {
    check(&Case::named("misc/dirname"));
}
