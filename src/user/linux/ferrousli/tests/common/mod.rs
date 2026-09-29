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
//!
//! # Another architecture
//!
//! The harness runs on the host, and can build the programs for another
//! architecture and run them under an emulator. Three variables say how:
//! `FERROUSLI_TEST_TARGET` is the Rust target the library and `crt1.o` are
//! built for, such as `aarch64-unknown-linux-gnu`; `CC` is a C compiler for
//! it; and `FERROUSLI_TEST_RUNNER` is the command each program runs under,
//! such as `qemu-aarch64`, its words split on spaces.

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
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// The Rust target the programs are built for, when it is not the host's.
fn target() -> Option<String> {
    std::env::var("FERROUSLI_TEST_TARGET")
        .ok()
        .filter(|target| !target.is_empty())
}

/// The command a program runs under, when it does not run natively. Its
/// first word is looked up in the harness's `PATH` here, since the program's
/// environment, which has none, is the one `Command` would search.
fn runner() -> Option<Vec<PathBuf>> {
    let words = std::env::var("FERROUSLI_TEST_RUNNER").ok()?;
    let mut words: Vec<PathBuf> = words.split_whitespace().map(PathBuf::from).collect();
    let first = words.first_mut()?;
    if first.components().count() == 1 {
        let path = std::env::var_os("PATH").unwrap_or_default();
        if let Some(found) = std::env::split_paths(&path)
            .map(|dir| dir.join(&*first))
            .find(|candidate| candidate.is_file())
        {
            *first = found;
        }
    }
    Some(words)
}

/// `crt1.o` for the programs' target: the build script's for the host, and
/// otherwise one this compiles once, as the build script does.
fn crt1() -> &'static Path {
    static CRT1: OnceLock<PathBuf> = OnceLock::new();
    CRT1.get_or_init(|| {
        let Some(target) = target() else {
            return PathBuf::from(env!("FERROUSLI_CRT1"));
        };
        let out = scratch().join(format!("crt1-{target}.o"));
        std::fs::create_dir_all(scratch()).expect("create the scratch directory");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let status = Command::new(rustc)
            .args([
                "--edition=2024",
                "--crate-type=lib",
                "--crate-name=crt1",
                "-Cpanic=abort",
                "--target",
                &target,
            ])
            .arg(format!("--emit=obj={}", out.display()))
            .arg(manifest().join("crt/crt1.rs"))
            .current_dir(manifest())
            .status()
            .expect("run rustc");
        assert!(
            status.success(),
            "rustc could not build crt/crt1.rs for {target}"
        );
        out
    })
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
        let mut cargo = Command::new(env!("CARGO"));
        let _ = cargo
            .args([
                "build",
                "--lib",
                "--profile",
                profile_name,
                "--manifest-path",
            ])
            .arg(manifest().join("Cargo.toml"));
        let Some(target) = target() else {
            let status = cargo.status().expect("run cargo");
            assert!(status.success(), "cargo could not build libferrousli.a");
            return profile.join("libferrousli.a");
        };
        let status = cargo
            .args(["--target", &target])
            .status()
            .expect("run cargo");
        assert!(
            status.success(),
            "cargo could not build libferrousli.a for {target}"
        );
        // A build for a target lands in a directory named for it, beside the
        // host's profile directories.
        profile
            .parent()
            .expect("the target directory")
            .join(target)
            .join(name)
            .join("libferrousli.a")
    })
}

/// A name usable as one path component.
fn flat(name: &str) -> String {
    name.replace('/', "-")
}

/// A directory of one build's own under `CARGO_TARGET_TMPDIR`, holding the
/// program and the directory it runs in, removed when this is dropped.
///
/// A test drops it when it returns and when it panics, so a run leaves nothing
/// behind; before this, every run of the suite left a program and its files
/// for each case at each optimisation level, gigabytes a day on a build host.
#[derive(Debug)]
pub(crate) struct Scratch {
    /// The directory.
    pub(crate) path: PathBuf,
}

impl Scratch {
    /// A new, empty directory for `case` at `opt`.
    ///
    /// Tests run in parallel, and two of them running one program with
    /// different arguments would otherwise write the same file while the other
    /// is executing it, so each gets a name no other build in any process
    /// shares. `CARGO_TARGET_TMPDIR` itself is created if something removed
    /// it: cargo only makes it when it builds the tests.
    fn new(case: &Case, opt: &str) -> Self {
        static BUILDS: AtomicUsize = AtomicUsize::new(0);
        let serial = BUILDS.fetch_add(1, Ordering::Relaxed);
        let path = scratch().join(format!(
            "{}{opt}-{}-{serial}",
            flat(case.name),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the build's directory");
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Compiles `case` at optimisation level `opt` into `dir`, and returns the
/// program.
pub(crate) fn build(case: &Case, opt: &str, dir: &Scratch) -> PathBuf {
    let source = manifest().join("tests/c").join(format!("{}.c", case.name));
    let out = dir.path.join(flat(case.name));
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
        .args(runner().map(|_| "-DFERROUSLI_TEST_EMULATED"))
        .arg(opt)
        .args(case.cflags)
        .arg("-o")
        .arg(&out)
        .arg(crt1())
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
    let mut command = match runner() {
        Some(words) => {
            let mut command = Command::new(words.first().expect("a runner"));
            let _ = command
                .args(words.get(1..).unwrap_or_default())
                .arg(program);
            command
        }
        None => Command::new(program),
    };
    let mut child = command
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

    // An emulator runs a program some ten times slower.
    let limit = if runner().is_some() {
        TIME_LIMIT * 10
    } else {
        TIME_LIMIT
    };
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll the program") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{label}: still running after {limit:?}");
        }
        thread::sleep(Duration::from_millis(5));
    };

    let _ = writer.join();
    let mut stderr = err_reader.join().unwrap_or_default();
    // qemu-user reports a program killed by a signal on standard error, as
    // the program's own last line, after dying of the signal itself.
    if runner().is_some()
        && let Some(at) = find(&stderr, b"qemu: uncaught target signal ")
    {
        stderr.truncate(at);
    }
    Finished {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr,
    }
}

/// Where `needle` first starts in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Builds `case` at `-O0` and `-O2`, runs each, and checks what it did. The
/// program and everything it wrote are removed afterwards, pass or fail.
pub(crate) fn check(case: &Case) {
    for opt in ["-O0", "-O2"] {
        let scratch = Scratch::new(case, opt);
        let program = build(case, opt, &scratch);
        let label = format!("{}{opt}", case.name);
        let dir = scratch.path.join("run");
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
