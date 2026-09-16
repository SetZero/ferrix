//! The C programs that exercise `stdio.h`: variadic arguments, buffering,
//! streams on files, memory and cookies, and the `printf` family.
//!
//! Most are built with `-fno-builtin`, so that the compiler cannot answer a
//! `snprintf` of constants itself and every check reaches the library. The
//! buffering program is built without it, so that the calls the compiler
//! rewrites, `printf` of a plain line into `puts`, are covered too. Several
//! programs are adapted from libc-test, and say so.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, Ending, SIGABRT, check};

/// Keeps the compiler from folding calls to the functions under test.
const NO_BUILTIN: &[&str] = &["-fno-builtin"];

/// A program in `tests/c/stdio`, built with [`NO_BUILTIN`].
const fn stdio(name: &'static str) -> Case {
    Case {
        cflags: NO_BUILTIN,
        ..Case::named(name)
    }
}

#[test]
fn variadic_arguments_past_the_registers_and_passed_on_lists() {
    check(&stdio("stdio/va"));
}

#[test]
fn libc_test_snprintf() {
    check(&stdio("stdio/snprintf"));
}

#[test]
fn libc_test_printf_regressions() {
    check(&stdio("stdio/printf_1e9_oob"));
    check(&stdio("stdio/printf_fmt_g_round"));
    check(&stdio("stdio/printf_fmt_g_zeros"));
    check(&stdio("stdio/printf_fmt_n"));
}

#[test]
fn printf_conversions_flags_positions_and_errors() {
    check(&Case {
        stdout: "printf done\n",
        ..stdio("stdio/printf")
    });
}

#[test]
fn glibcs_checked_printf_functions() {
    check(&Case {
        stdout: "chk 1\nfchk 2\n",
        ..stdio("stdio/fortify")
    });
}

#[test]
fn a_checked_printf_past_its_object_aborts() {
    for which in ["sprintf", "snprintf"] {
        let args: &'static [&'static str] = if which == "sprintf" {
            &["sprintf"]
        } else {
            &["snprintf"]
        };
        check(&Case {
            args,
            stderr: "*** buffer overflow detected ***: terminated\n",
            ending: Ending::Signal(SIGABRT),
            ..stdio("stdio/fortify")
        });
    }
}

/// The buffering program, run with one argument.
const fn buffering(arg: &'static [&'static str]) -> Case {
    Case {
        args: arg,
        ..Case::named("stdio/buffering")
    }
}

#[test]
fn standard_output_on_a_pipe_is_fully_buffered() {
    check(&Case {
        stdout: "raw\nbuffered\n",
        ..buffering(&["full"])
    });
}

#[test]
fn a_line_buffered_stream_flushes_at_each_newline() {
    check(&Case {
        stdout: "line\n|raw|partial\n",
        ..buffering(&["line"])
    });
}

#[test]
fn an_unbuffered_stream_writes_every_call() {
    check(&Case {
        stdout: "abcde\n",
        ..buffering(&["none"])
    });
}

#[test]
fn standard_error_is_unbuffered() {
    check(&Case {
        stdout: "out ",
        stderr: "err\nraw\n",
        ..buffering(&["stderr"])
    });
}

#[test]
fn exit_flushes_output_after_the_handlers_and_underscore_exit_does_not() {
    check(&Case {
        stdout: "x",
        ending: Ending::Code(3),
        ..buffering(&["exit"])
    });
    check(&Case {
        ending: Ending::Code(4),
        ..buffering(&["_exit"])
    });
    check(&Case {
        stdout: "main\nhandler\n",
        ..buffering(&["atexit"])
    });
}

#[test]
fn libc_test_stream_functions() {
    check(&stdio("stdio/fdopen"));
    check(&stdio("stdio/memstream"));
    check(&stdio("stdio/ungetc"));
}

#[test]
fn libc_test_stream_regressions() {
    check(&stdio("stdio/fgets_eof"));
    check(&stdio("stdio/ftello_unflushed_append"));
    check(&stdio("stdio/rewind_clear_error"));
    check(&stdio("stdio/setvbuf_unget"));
    check(&stdio("stdio/fflush_exit"));
}

#[test]
fn files_modes_positions_lines_cookies_and_locks() {
    check(&stdio("stdio/files"));
}

#[test]
fn standard_input_standard_error_and_perror() {
    check(&Case {
        stdin: "first line\nsecond\nthird\nrest of it",
        stdout: "done\n",
        stderr: "stdin: Permission denied\nNo error information\nNo such file or directory\nto stderr\n",
        ..stdio("stdio/stdin")
    });
}

#[test]
fn scanf_reads_numbers_strings_and_sets_from_strings_and_streams() {
    check(&stdio("stdio/scanf"));
}

#[test]
fn wide_characters_read_and_write_utf_8_and_report_bad_sequences() {
    check(&stdio("stdio/wide"));
}

#[test]
fn wprintf_counts_wide_characters_and_writes_utf_8() {
    check(&stdio("stdio/wprintf"));
}
