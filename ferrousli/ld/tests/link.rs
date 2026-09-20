//! The loader, against a dynamically linked program built by the host's own
//! compiler.
//!
//! # Why the fixtures carry no C library
//!
//! `tests/c/prog.c` and `tests/c/greet.c` are built with `-nostdlib` and make
//! their one system call in assembly. That is deliberate: a fixture linked
//! against a C library would prove the loader *and* that library, and when it
//! failed there would be two places to look. With no library in the picture,
//! anything that goes wrong is the loader's.
//!
//! # Why the checks are exit codes
//!
//! The program has no way to print: there is no `stdio` and no C library. So
//! it answers with its exit status, and each check has a number of its own —
//! a failure says which relocation was not written, not merely that one was
//! not.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test reports failure by panicking"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// What `prog.c` exits with when every relocation was written.
const EXPECTED: i32 = 42;

/// The target the loader is built for: a Linux program, statically linked and
/// position-independent, which is the shape a dynamic loader has to have.
const TARGET: &str = "x86_64-unknown-linux-musl";

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn scratch() -> &'static Path {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
}

/// Build the loader, and answer where it landed.
///
/// `cargo test` does not build it: the loader is a binary for another target
/// than the tests, so the harness asks cargo for it, as `tests/common` asks
/// for `libferrousli.a`.
fn loader() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--bin",
            "ld-ferrousli",
            "--features",
            "loader",
            "--target",
            TARGET,
            "--manifest-path",
        ])
        .arg(manifest().join("Cargo.toml"))
        .status()
        .expect("run cargo");
    assert!(status.success(), "cargo could not build the loader");
    manifest()
        .join("../target")
        .join(TARGET)
        .join("debug")
        .join("ld-ferrousli")
        .canonicalize()
        .expect("the loader was built")
}

/// Compile a fixture, with the arguments that make it freestanding.
fn compile(args: &[&str]) {
    let status = Command::new("cc")
        .args(["-nostdlib", "-fPIC", "-O1"])
        .args(args)
        .status()
        .expect("run cc");
    assert!(status.success(), "cc could not build a fixture: {args:?}");
}

#[test]
fn a_dynamically_linked_program_runs_under_the_loader() {
    let loader = loader();
    let dir = scratch().join("ld-link");
    std::fs::create_dir_all(&dir).expect("make the scratch directory");

    let library = dir.join("libgreet.so");
    compile(&[
        "-shared",
        "-o",
        library.to_str().expect("a path"),
        manifest().join("tests/c/greet.c").to_str().expect("a path"),
    ]);

    // The library is named by an absolute path, so the loader uses it as it
    // is rather than searching for it. What the search path does is a
    // question for the guest, where there is a `/lib` to search.
    let program = dir.join("prog");
    compile(&[
        "-pie",
        "-o",
        program.to_str().expect("a path"),
        manifest().join("tests/c/prog.c").to_str().expect("a path"),
        library.to_str().expect("a path"),
        &format!("-Wl,--dynamic-linker={}", loader.display()),
        "-Wl,-e,_start",
    ]);

    let status = Command::new(&program).status().expect("run the program");
    assert_eq!(
        status.code(),
        Some(EXPECTED),
        "a dynamically linked program did not run under the loader; \
         91 is the library's datum, 92 a function pointer in the program's \
         data, 93 a pointer into the library's data, and a signal is the \
         loader or the program faulting"
    );
}

#[test]
fn the_loader_run_as_a_program_says_how_to_use_it() {
    let output = Command::new(loader()).output().expect("run the loader");
    assert_eq!(output.status.code(), Some(127), "the code a shell reports");
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        said.contains("usage"),
        "the loader run with no program should say how to use it, said {said:?}"
    );
}
