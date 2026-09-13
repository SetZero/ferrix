//! The harness every C program test shares.
//!
//! A test names a program in `tests/c`. The harness compiles it with the host's
//! `cc` against `include/`, `crt1.o` and `libferrousli.a`, and nothing from the
//! host's C library. It runs the program in a fresh directory with only the
//! arguments, environment and input the test gives, and checks what it printed
//! and how it ended.
//!
//! This is the test that matters for a C library. Unit tests call the functions
//! from Rust. Only a C program exercises the entry path, the exported names and
//! the calling convention.
//!
//! Each program is built at `-O0` and at `-O2`, because the optimiser turns
//! loops and assignments into calls to `memcpy` and `memset` that the source
//! never made. The stack protector is on, as a distribution's compiler leaves
//! it.

#![allow(
    dead_code,
    reason = "each test crate uses a different part of the harness"
)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test reports failure by panicking"
)]

use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

/// `SIGABRT`.
pub(crate) const SIGABRT: i32 = 6;
/// `SIGSEGV`.
pub(crate) const SIGSEGV: i32 = 11;

/// How long a program may run before the test kills it and fails.
const TIME_LIMIT: Duration = Duration::from_secs(30);

/// How a program must end.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Ending {
    /// It exits with this status.
    Code(i32),
    /// It is killed by this signal.
    Signal(i32),
}

/// One program, how it is built and run, and what it must do.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Case {
    /// The source file under `tests/c`, without `.c`. It may name a
    /// subdirectory, as in `"malloc/basic"`.
    pub(crate) name: &'static str,
    /// Compiler flags beyond the usual ones.
    pub(crate) cflags: &'static [&'static str],
    pub(crate) args: &'static [&'static str],
    /// The whole environment. Nothing is inherited.
    pub(crate) env: &'static [(&'static str, &'static str)],
    /// Written to the program's standard input, which is then closed.
    pub(crate) stdin: &'static str,
    pub(crate) stdout: &'static str,
    pub(crate) stderr: &'static str,
    pub(crate) ending: Ending,
}

impl Case {
    /// A program built with the usual flags and run with no arguments, no
    /// environment and no input, which prints nothing and exits zero.
    pub(crate) const fn named(name: &'static str) -> Self {
        Self {
            name,
            cflags: &[],
            args: &[],
            env: &[],
            stdin: "",
            stdout: "",
            stderr: "",
            ending: Ending::Code(0),
        }
    }
}

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn scratch() -> &'static Path {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
}

/// `libferrousli.a`, built once for every test in the crate.
///
/// `cargo test` does not produce it: it builds the library as a unit test
/// binary, and builds no static library for integration tests to link, since
/// they can only link an rlib. So the harness asks cargo for it, with the
/// profile this test was built with. Its output lands beside the directory
/// holding this test's executable.
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
            .arg(manifest().join("Cargo.toml"))
            .status()
            .expect("run cargo");
        assert!(status.success(), "cargo could not build libferrousli.a");
        profile.join("libferrousli.a")
    })
}

/// A name usable as one path component.
fn flat(name: &str) -> String {
    name.replace('/', "-")
}

/// Compiles `case` at optimisation level `opt`, and returns the program.
pub(crate) fn build(case: &Case, opt: &str) -> PathBuf {
    let source = manifest().join("tests/c").join(format!("{}.c", case.name));
    let out = scratch().join(format!("{}{opt}", flat(case.name)));
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
        .arg(manifest().join("include"))
        // For `check.h`.
        .arg("-I")
        .arg(manifest().join("tests/c"))
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

/// What a finished program did.
struct Finished {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Runs `program` in `dir`, feeding it `case.stdin`, and kills it if it
/// outlives [`TIME_LIMIT`].
///
/// Output is collected by threads that read until the pipes close. A program
/// that leaves a child holding them open keeps the test waiting until that
/// child exits.
fn run(program: &Path, case: &Case, dir: &Path, label: &str) -> Finished {
    let mut child = Command::new(program)
        .args(case.args)
        .env_clear()
        .envs(case.env.iter().copied())
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the program");

    let mut stdin = child.stdin.take().expect("the program's standard input");
    let mut stdout = child.stdout.take().expect("the program's standard output");
    let mut stderr = child.stderr.take().expect("the program's standard error");
    let input = case.stdin.as_bytes();
    // A program that does not read its input closes the pipe; the error that
    // leaves the writer with is not the test's concern.
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(input);
    });
    let out_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });

    let deadline = Instant::now() + TIME_LIMIT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll the program") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{label}: still running after {TIME_LIMIT:?}");
        }
        thread::sleep(Duration::from_millis(5));
    };

    let _ = writer.join();
    Finished {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    }
}

/// Builds `case` at `-O0` and `-O2`, runs each, and checks what it did.
pub(crate) fn check(case: &Case) {
    for opt in ["-O0", "-O2"] {
        let program = build(case, opt);
        let label = format!("{}{opt}", case.name);
        let dir = scratch().join(format!("{}{opt}.dir", flat(case.name)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the program's directory");

        let finished = run(&program, case, &dir, &label);
        assert_eq!(
            String::from_utf8_lossy(&finished.stdout),
            case.stdout,
            "{label}: standard output"
        );
        assert_eq!(
            String::from_utf8_lossy(&finished.stderr),
            case.stderr,
            "{label}: standard error"
        );
        match case.ending {
            Ending::Code(code) => assert_eq!(
                finished.status.code(),
                Some(code),
                "{label}: exit status (killed by signal {:?})",
                finished.status.signal()
            ),
            Ending::Signal(signal) => assert_eq!(
                finished.status.signal(),
                Some(signal),
                "{label}: signal (exited with {:?})",
                finished.status.code()
            ),
        }
    }
}
