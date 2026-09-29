//! What a compound command does with the processes it starts.
//!
//! `if`, `while`, `{ }` and a function's body run in the shell itself rather
//! than in a process of the job the compound command belongs to. Every
//! pipeline written in one is therefore a job of its own, which the shell
//! waits for before it runs the next -- and the day it was not, oh-my-zsh's
//! installer ran `cd "$ZSH"` while the `git init "$ZSH"` before its `&&` was
//! still starting, and said the directory did not exist.

use std::path::PathBuf;
use std::process::Command;

/// What `zinc -c script` printed on standard output, with what it exited.
///
/// A shell that could not be started comes back as its own message rather
/// than as a panic, so the assertion below is what reports it.
fn run(script: &str) -> (String, i32) {
    match Command::new(env!("CARGO_BIN_EXE_zinc"))
        .args(["-c", script])
        .output()
    {
        Ok(output) => (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            output.status.code().unwrap_or(-1),
        ),
        Err(error) => (format!("zinc did not start: {error}"), -1),
    }
}

/// A directory of this test's own, named after the test, removed first so a
/// run left over from a failure does not decide the next one.
fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("zinc-{name}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    let made = std::fs::create_dir_all(&path);
    assert!(made.is_ok(), "could not make {}: {made:?}", path.display());
    path
}

#[test]
fn a_command_in_a_function_has_finished_before_the_next_one_starts() {
    let dir = scratch("function-order");
    let made = dir.join("made");
    let script = format!(
        "f() {{ mkdir {0} && cd {0} && echo in $PWD; }}; f",
        made.display()
    );
    let (out, status) = run(&script);
    assert_eq!(out, format!("in {}\n", made.display()));
    assert_eq!(status, 0);
    let _gone = std::fs::remove_dir_all(&dir);
}

/// The same for the other three, since each runs its body the same way.
#[test]
fn the_other_compound_commands_wait_as_well() {
    for (name, shape) in [
        ("brace", "{ CMD }"),
        ("if", "if true; then CMD fi"),
        ("while", "while true; do CMD break; done"),
    ] {
        let dir = scratch(name);
        let made = dir.join("made");
        let body = format!("mkdir {0} && cd {0} && echo in $PWD;", made.display());
        let (out, status) = run(&shape.replace("CMD", &body));
        assert_eq!(out, format!("in {}\n", made.display()), "in {name}");
        assert_eq!(status, 0, "in {name}");
        let _gone = std::fs::remove_dir_all(&dir);
    }
}

/// `$?` is the status the process exited with, which means it was waited for.
#[test]
fn a_status_in_a_function_is_the_one_the_process_exited_with() {
    let (out, _status) = run("f() { /bin/sh -c 'exit 3'; echo status=$?; }; f");
    assert_eq!(out, "status=3\n");
}

/// What a forked command writes arrives before what the shell writes after
/// it: the shell was waiting rather than running on.
#[test]
fn output_comes_out_in_the_order_it_was_written() {
    let (out, _status) = run("f() { /bin/echo one; echo two; /bin/echo three; }; f");
    assert_eq!(out, "one\ntwo\nthree\n");
}
