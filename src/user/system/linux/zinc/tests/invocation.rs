//! How zinc reads its own arguments: `-c` is a flag, and the command is the
//! first operand after the options, as POSIX and zsh have it.
//!
//! glibc's `system` and `popen` run `/bin/sh -c -- CMD`, and with zinc as
//! `/bin/sh` the `--` was taken for the command -- "command not found: --"
//! -- so `system("exit 3")` answered 32512 and `popen` read nothing
//! (ferrix-ea's black-box pass, 2026-09-26).

use std::process::Command;

/// What zinc printed on standard output, and what it exited with.
fn zinc(args: &[&str]) -> (String, i32) {
    match Command::new(env!("CARGO_BIN_EXE_zinc")).args(args).output() {
        Ok(output) => (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            output.status.code().unwrap_or(-1),
        ),
        Err(error) => (format!("zinc did not start: {error}"), -1),
    }
}

#[test]
fn a_double_dash_after_c_ends_the_options() {
    assert_eq!(
        zinc(&["-c", "--", "echo ran; exit 3"]),
        ("ran\n".to_owned(), 3)
    );
}

#[test]
fn the_operands_after_the_command_are_its_name_and_arguments() {
    let (out, status) = zinc(&["-c", "--", "echo $0 $1 $2", "name", "one", "two"]);
    assert_eq!((out.as_str(), status), ("name one two\n", 0));
}

#[test]
fn c_clusters_with_other_flags() {
    assert_eq!(zinc(&["-fc", "echo ran"]), ("ran\n".to_owned(), 0));
}

#[test]
fn c_without_a_string_is_an_error() {
    assert_eq!(zinc(&["-c"]).1, 2);
}
