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

/// Build the C runtime pieces that make `crt1.o` pass the loader's exit
/// callback to `__libc_start_main`.
fn runtime(dir: &Path) -> (PathBuf, PathBuf) {
    let root = manifest().parent().expect("the loader has a parent");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut build = Command::new(env!("CARGO"));
    let _ = build.args(["build", "--lib", "--manifest-path"]);
    let _ = build.arg(root.join("Cargo.toml"));
    if profile == "release" {
        let _ = build.arg("--release");
    }
    let status = build.status().expect("build the C runtime");
    assert!(status.success(), "cargo could not build libferrousli.a");

    let crt = dir.join("crt1.o");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(rustc)
        .current_dir(root)
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--crate-name=crt1",
            "-Cpanic=abort",
        ])
        .arg(format!("--emit=obj={}", crt.display()))
        .arg("crt/crt1.rs")
        .status()
        .expect("build crt1.o");
    assert!(status.success(), "rustc could not build crt1.o");
    (crt, root.join("target").join(profile).join("libferrousli.a"))
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
    // is rather than searching for it. The test below covers the search path.
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

/// `DT_RUNPATH` finds a directly needed library before the system defaults.
///
/// The library lives in a new temporary directory, not beside the program and
/// not in `/lib`, and `LD_LIBRARY_PATH` is removed. The program can therefore
/// reach its expected status only when the loader reads its own `DT_RUNPATH`,
/// splits it into directories and maps the `DT_NEEDED` name it finds there.
#[test]
fn a_program_finds_a_library_through_its_runpath() {
    let loader = loader();
    let dir = scratch().join("ld-runpath");
    let libraries = dir.join("libraries");
    std::fs::create_dir_all(&libraries).expect("make the library directory");

    let library = libraries.join("libgreet.so");
    compile(&[
        "-shared",
        "-Wl,-soname,libgreet.so",
        "-o",
        library.to_str().expect("a path"),
        manifest().join("tests/c/greet.c").to_str().expect("a path"),
    ]);

    let program = dir.join("prog");
    compile(&[
        "-pie",
        "-o",
        program.to_str().expect("a path"),
        manifest().join("tests/c/prog.c").to_str().expect("a path"),
        "-L",
        libraries.to_str().expect("a path"),
        "-lgreet",
        &format!("-Wl,-rpath,{}", libraries.display()),
        "-Wl,--enable-new-dtags",
        &format!("-Wl,--dynamic-linker={}", loader.display()),
        "-Wl,-e,_start",
    ]);

    let status = Command::new(&program)
        .env_remove("LD_LIBRARY_PATH")
        .status()
        .expect("run the program");
    assert_eq!(
        status.code(),
        Some(EXPECTED),
        "a program did not find its DT_NEEDED library through DT_RUNPATH"
    );
}

/// `PT_TLS` images are copied before the program starts, and x86-64's
/// initial-exec `R_X86_64_TPOFF64` points a library at its own block.
///
/// The two objects have distinct initial values. The library is called twice,
/// so this catches both a missing image copy and an offset into the program's
/// block rather than the library's.
#[test]
fn initial_exec_tls_works_in_a_program_and_its_library() {
    let loader = loader();
    let dir = scratch().join("ld-tls");
    std::fs::create_dir_all(&dir).expect("make the scratch directory");

    let library = dir.join("libtls.so");
    compile(&[
        "-shared",
        "-ftls-model=initial-exec",
        "-o",
        library.to_str().expect("a path"),
        manifest().join("tests/c/tls_greet.c").to_str().expect("a path"),
    ]);

    let program = dir.join("prog");
    compile(&[
        "-pie",
        "-ftls-model=initial-exec",
        "-o",
        program.to_str().expect("a path"),
        manifest().join("tests/c/tls_prog.c").to_str().expect("a path"),
        library.to_str().expect("a path"),
        &format!("-Wl,--dynamic-linker={}", loader.display()),
        "-Wl,-e,_start",
    ]);

    let status = Command::new(&program).status().expect("run the program");
    assert_eq!(
        status.code(),
        Some(EXPECTED),
        "initial-exec TLS did not reach its program and library blocks"
    );
}

/// The C runtime receives and registers the loader's `rtld_fini` callback.
///
/// The shared library's destructor writes after `main` returns. That is only
/// observable if `crt1.o` passes `rdx` to `__libc_start_main`, the runtime
/// registers it with `atexit`, and the loader reads and calls its
/// `DT_FINI_ARRAY` entry.
#[test]
fn a_runtime_runs_a_dependency_fini_array_at_exit() {
    let loader = loader();
    let dir = scratch().join("ld-fini");
    std::fs::create_dir_all(&dir).expect("make the scratch directory");
    let (crt, runtime) = runtime(&dir);

    let library = dir.join("libfini.so");
    compile(&[
        "-shared",
        "-o",
        library.to_str().expect("a path"),
        manifest().join("tests/c/fini.c").to_str().expect("a path"),
    ]);

    let program = dir.join("prog");
    let status = Command::new("cc")
        .args(["-nostdlib", "-fPIC", "-O1", "-pie", "-o"])
        .arg(&program)
        .arg(&crt)
        .arg(manifest().join("tests/c/fini_prog.c"))
        .arg(&runtime)
        .arg(&library)
        .arg(format!("-Wl,--dynamic-linker={}", loader.display()))
        .arg("-Wl,-e,_start")
        .status()
        .expect("build the dynamically linked C program");
    assert!(status.success(), "cc could not build the C runtime fixture");

    let output = Command::new(&program).output().expect("run the program");
    assert_eq!(output.status.code(), Some(EXPECTED));
    assert_eq!(output.stdout, b"main\nfini\n");
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
