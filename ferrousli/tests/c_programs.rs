//! Builds small C programs against the library and runs them.
//!
//! This is the test that matters for a C library. The unit tests call the
//! functions from Rust. Only a C program linked with `crt1.o` and
//! `libferrousli.a`, and nothing from the host's libc, exercises the entry
//! path, the exported names and the calling convention.
//!
//! The programs are compiled against `include/`, not the host's headers. Each
//! is built at `-O0` and at `-O2`, because the optimiser turns loops and
//! assignments into calls to `memcpy` and `memset` that the source never made.
//! The stack protector is on, as a distribution's compiler leaves it.

#![allow(clippy::expect_used, reason = "a test reports failure by panicking")]

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// `SIGABRT`.
const SIGABRT: i32 = 6;

/// How a program must end.
#[derive(Debug, Clone, Copy)]
enum Ending {
    /// It exits with this status.
    Code(i32),
    /// It is killed by this signal.
    Signal(i32),
}

/// One program, how it is built and run, and what it must do.
struct Case {
    /// The source file in `tests/c`, without `.c`.
    name: &'static str,
    /// Compiler flags beyond the usual ones.
    cflags: &'static [&'static str],
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
    stdout: &'static str,
    stderr: &'static str,
    ending: Ending,
}

impl Case {
    /// A program built with the usual flags and run with no arguments and no
    /// environment, which prints nothing and exits zero.
    const fn named(name: &'static str) -> Self {
        Self {
            name,
            cflags: &[],
            args: &[],
            env: &[],
            stdout: "",
            stderr: "",
            ending: Ending::Code(0),
        }
    }
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

fn build(case: &Case, opt: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest.join("tests/c").join(format!("{}.c", case.name));
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{}{opt}", case.name));
    let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let output = Command::new(cc)
        .args([
            "-std=c11",
            "-Wall",
            "-Werror",
            "-static",
            "-no-pie",
            "-nostdlib",
            "-nostdinc",
            "-isystem",
        ])
        .arg(manifest.join("include"))
        .arg("-fstack-protector-strong")
        .arg(opt)
        .args(case.cflags)
        .arg("-o")
        .arg(&out)
        .arg(env!("FERROUSLI_CRT1"))
        .arg(&source)
        .arg(library())
        .output()
        .expect("run the C compiler");
    assert!(
        output.status.success(),
        "{}{opt} did not build:\n{}",
        case.name,
        String::from_utf8_lossy(&output.stderr)
    );
    out
}

fn check(case: &Case) {
    for opt in ["-O0", "-O2"] {
        let program = build(case, opt);
        let output = Command::new(&program)
            .args(case.args)
            .env_clear()
            .envs(case.env.iter().copied())
            .output()
            .expect("run the program");
        let label = format!("{}{opt}", case.name);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            case.stdout,
            "{label}: standard output"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            case.stderr,
            "{label}: standard error"
        );
        match case.ending {
            Ending::Code(code) => assert_eq!(
                output.status.code(),
                Some(code),
                "{label}: exit status (killed by signal {:?})",
                output.status.signal()
            ),
            Ending::Signal(signal) => assert_eq!(
                output.status.signal(),
                Some(signal),
                "{label}: signal (exited with {:?})",
                output.status.code()
            ),
        }
    }
}

#[test]
fn hello() {
    check(&Case {
        stdout: "hello, world\n",
        ..Case::named("hello")
    });
}

#[test]
fn arguments_environment_and_constructors_reach_main() {
    check(&Case {
        args: &["one", "two words"],
        env: &[("FERROUSLI_TEST", "a value")],
        stdout: "one\ntwo words\na value\n",
        ending: Ending::Code(3),
        ..Case::named("startup")
    });
}

#[test]
fn string_and_memory_functions() {
    check(&Case::named("strings"));
}

#[test]
fn a_failed_call_sets_errno() {
    check(&Case::named("errno"));
}

#[test]
fn thread_local_storage_in_the_static_block() {
    check(&Case::named("tls"));
}

#[test]
fn thread_local_storage_too_large_for_the_static_block() {
    check(&Case::named("tls_large"));
}

#[test]
fn exit_runs_handlers_newest_first_then_destructors() {
    check(&Case {
        stdout: "main\nsecond\nfirst\ndestructor\n",
        ending: Ending::Code(7),
        ..Case::named("exit_handlers")
    });
}

#[test]
fn the_auxiliary_vector_reaches_getauxval() {
    check(&Case::named("auxv"));
}

#[test]
fn the_stack_protector_has_a_canary_and_catches_a_smashed_stack() {
    const ALL: &[&str] = &["-fstack-protector-all"];
    check(&Case {
        cflags: ALL,
        ..Case::named("canary")
    });
    check(&Case {
        cflags: ALL,
        args: &["smash"],
        stderr: "*** stack smashing detected ***: terminated\n",
        ending: Ending::Signal(SIGABRT),
        ..Case::named("canary")
    });
}

#[test]
fn abort_ends_the_process_with_sigabrt() {
    check(&Case {
        ending: Ending::Signal(SIGABRT),
        ..Case::named("abort")
    });
}
