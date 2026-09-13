//! Builds small C programs against the library and runs them.
//!
//! This is the test that matters for a C library. The unit tests call the
//! functions from Rust. Only a C program linked with `crt1.o` and
//! `libferrousli.a`, and nothing from the host's libc, exercises the entry
//! path, the exported names and the calling convention.
//!
//! Each program is built at `-O0` and at `-O2`, because the optimiser turns
//! loops and assignments into calls to `memcpy` and `memset` that the source
//! never made.

#![allow(clippy::expect_used, reason = "a test reports failure by panicking")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// One program, how it is run, and what it must do.
struct Case {
    /// The source file in `tests/c`, without `.c`.
    name: &'static str,
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
    stdout: &'static str,
    status: i32,
}

/// `libferrousli.a`, built once for every test in this file.
///
/// `cargo test` does not produce it: it builds the library as a unit test
/// binary, and builds no static library for integration tests to link, since
/// they can only link an rlib. So the test asks cargo for it, with the profile
/// this test was built with. Its output lands beside the directory holding
/// this test's executable.
fn library() -> &'static Path {
    static LIBRARY: OnceLock<PathBuf> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        let exe = std::env::current_exe().expect("the test executable's path");
        let profile = exe
            .parent()
            .and_then(Path::parent)
            .expect("the target profile directory");
        let name = profile
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a profile directory name");
        let profile_name = if name == "debug" { "dev" } else { name };
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--lib",
                "--profile",
                profile_name,
                "--manifest-path",
            ])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .status()
            .expect("run cargo");
        assert!(status.success(), "cargo could not build libferrousli.a");
        profile.join("libferrousli.a")
    })
}

fn build(name: &str, opt: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/c")
        .join(format!("{name}.c"));
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}{opt}"));
    let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let output = Command::new(cc)
        .args([
            "-static",
            "-no-pie",
            "-nostdlib",
            "-nostdinc",
            // The canary lives at %fs:0x28, and there is no thread pointer to
            // put it behind yet.
            "-fno-stack-protector",
            opt,
            "-o",
        ])
        .arg(&out)
        .arg(env!("FERROUSLI_CRT1"))
        .arg(&source)
        .arg(library())
        .output()
        .expect("run the C compiler");
    assert!(
        output.status.success(),
        "{name}{opt} did not build:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    out
}

fn check(case: &Case) {
    for opt in ["-O0", "-O2"] {
        let program = build(case.name, opt);
        let output = Command::new(&program)
            .args(case.args)
            .env_clear()
            .envs(case.env.iter().copied())
            .output()
            .expect("run the program");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            case.stdout,
            "{}{opt}: standard output",
            case.name
        );
        // A program killed by a signal has no exit code, and shows as `None`.
        assert_eq!(
            output.status.code(),
            Some(case.status),
            "{}{opt}: exit status",
            case.name
        );
    }
}

#[test]
fn hello() {
    check(&Case {
        name: "hello",
        args: &[],
        env: &[],
        stdout: "hello, world\n",
        status: 0,
    });
}

#[test]
fn arguments_environment_and_constructors_reach_main() {
    check(&Case {
        name: "startup",
        args: &["one", "two words"],
        env: &[("FERROUSLI_TEST", "a value")],
        stdout: "one\ntwo words\na value\n",
        status: 3,
    });
}

#[test]
fn string_and_memory_functions() {
    check(&Case {
        name: "strings",
        args: &[],
        env: &[],
        stdout: "",
        status: 0,
    });
}

#[test]
fn a_failed_call_sets_errno() {
    check(&Case {
        name: "errno",
        args: &[],
        env: &[],
        stdout: "",
        status: 0,
    });
}
