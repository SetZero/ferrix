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
    target_dir()
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
    // `--out-dir` as well as the object's path: rustc writes its intermediate
    // files to the output directory, which is otherwise the current one, and
    // two tests building `crt1` at once in the same directory trip over each
    // other's. The current directory stays the crate's, where rustup finds
    // the toolchain the repository pins.
    let status = Command::new(rustc)
        .current_dir(root)
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--crate-name=crt1",
            "-Cpanic=abort",
        ])
        .arg("--out-dir")
        .arg(dir)
        .arg(format!("--emit=obj={}", crt.display()))
        .arg("crt/crt1.rs")
        .status()
        .expect("build crt1.o");
    assert!(status.success(), "rustc could not build crt1.o");
    (crt, target_dir().join(profile).join("libferrousli.a"))
}

/// Where cargo puts what it builds: `CARGO_TARGET_DIR` when the caller set
/// one, which `cargo xtask check` does, and the workspace's own `target`
/// otherwise.
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| manifest().join("../target"), PathBuf::from)
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

/// Every page of a library's `.bss` is mapped writable.
///
/// The part of a writable segment past the file is anonymous memory, mapped
/// from the page after the file's last byte. The loader once counted the
/// segment's offset within its first page twice there, and when that crossed
/// a page boundary it left a page of `.bss` inaccessible. Where the file's
/// part ends depends on how much initialised data precedes the `.bss`, so the
/// library is built at sizes that put it all round a page.
#[test]
fn every_page_of_a_librarys_bss_is_writable() {
    let loader = loader();
    let dir = scratch().join("ld-bss");
    std::fs::create_dir_all(&dir).expect("make the scratch directory");
    for pad in [1, 512, 1024, 1536, 2048, 2560, 3072, 3584] {
        let library = dir.join(format!("libbss{pad}.so"));
        compile(&[
            "-shared",
            &format!("-DPAD={pad}"),
            "-o",
            library.to_str().expect("a path"),
            manifest().join("tests/c/bss.c").to_str().expect("a path"),
        ]);
        let program = dir.join(format!("prog{pad}"));
        compile(&[
            "-pie",
            "-o",
            program.to_str().expect("a path"),
            manifest()
                .join("tests/c/bss_prog.c")
                .to_str()
                .expect("a path"),
            library.to_str().expect("a path"),
            &format!("-Wl,--dynamic-linker={}", loader.display()),
            "-Wl,-e,_start",
        ]);
        let status = Command::new(&program).status().expect("run the program");
        assert_eq!(
            status.code(),
            Some(EXPECTED),
            "with {pad} bytes of data before it, the library's .bss was not all \
             writable (a signal is a fault in it)"
        );
    }
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
        manifest()
            .join("tests/c/tls_greet.c")
            .to_str()
            .expect("a path"),
    ]);

    let program = dir.join("prog");
    compile(&[
        "-pie",
        "-ftls-model=initial-exec",
        "-o",
        program.to_str().expect("a path"),
        manifest()
            .join("tests/c/tls_prog.c")
            .to_str()
            .expect("a path"),
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

/// A library's TLS read by the general-dynamic and local-dynamic models, in
/// both of x86-64's dialects: `__tls_get_addr`, and TLS descriptors.
///
/// The program reads the library's variable by initial-exec as well, and the
/// two addresses must be one: that is what shows `DTPMOD`, `DTPOFF` and
/// `TLSDESC` were answered from the same layout `TPOFF` was.
#[test]
fn general_dynamic_tls_agrees_with_initial_exec_in_both_dialects() {
    let loader = loader();
    let dir = scratch().join("ld-tls-gd");
    std::fs::create_dir_all(&dir).expect("make the scratch directory");
    for dialect in ["gnu", "gnu2"] {
        let library = dir.join(format!("libtlsgd-{dialect}.so"));
        compile(&[
            "-shared",
            &format!("-mtls-dialect={dialect}"),
            "-o",
            library.to_str().expect("a path"),
            manifest()
                .join("tests/c/tls_gd.c")
                .to_str()
                .expect("a path"),
        ]);
        let program = dir.join(format!("prog-{dialect}"));
        compile(&[
            "-pie",
            "-o",
            program.to_str().expect("a path"),
            manifest()
                .join("tests/c/tls_gd_prog.c")
                .to_str()
                .expect("a path"),
            library.to_str().expect("a path"),
            &format!("-Wl,--dynamic-linker={}", loader.display()),
            "-Wl,-e,_start",
        ]);
        let status = Command::new(&program).status().expect("run the program");
        assert_eq!(
            status.code(),
            Some(EXPECTED),
            "general-dynamic TLS failed in the {dialect} dialect: 91 is the \
             value, 92 the address disagreeing with initial-exec's, 93 the \
             local-dynamic block, and a signal a fault"
        );
    }
}

/// `dlfcn.h` against the loader alone: a library opened at run time, its
/// function and datum found by handle and globally, an address named, every
/// object listed, a failure reported once, and a TLS library's variables
/// reached from a thread that was running before it was loaded.
///
/// The program links against a stub whose `SONAME` is the loader's file
/// name, so its `DT_NEEDED` names the loader, which the loader takes for
/// itself; the calls then bind to the loader's own exports.
#[test]
fn dlopen_dlsym_dladdr_and_dl_iterate_phdr_work_through_the_loader() {
    let loader = loader();
    let dir = scratch().join("ld-dl");
    std::fs::create_dir_all(&dir).expect("make the scratch directory");

    let library = dir.join("libgreet.so");
    compile(&[
        "-shared",
        "-o",
        library.to_str().expect("a path"),
        manifest().join("tests/c/greet.c").to_str().expect("a path"),
    ]);
    let tls_library = dir.join("libtls.so");
    compile(&[
        "-shared",
        "-o",
        tls_library.to_str().expect("a path"),
        manifest()
            .join("tests/c/tls_gd.c")
            .to_str()
            .expect("a path"),
    ]);
    let loader_name = loader
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the loader has a file name");
    let stub = dir.join("libdlstub.so");
    compile(&[
        "-shared",
        &format!("-Wl,-soname,{loader_name}"),
        "-o",
        stub.to_str().expect("a path"),
        manifest()
            .join("tests/c/dlstub.c")
            .to_str()
            .expect("a path"),
    ]);

    let program = dir.join("prog");
    compile(&[
        "-pie",
        &format!("-DLIBRARY=\"{}\"", library.display()),
        &format!("-DTLS_LIBRARY=\"{}\"", tls_library.display()),
        "-o",
        program.to_str().expect("a path"),
        manifest()
            .join("tests/c/dl_prog.c")
            .to_str()
            .expect("a path"),
        stub.to_str().expect("a path"),
        &format!("-Wl,--dynamic-linker={}", loader.display()),
        "-Wl,-e,_start",
    ]);

    let status = Command::new(&program).status().expect("run the program");
    assert_eq!(
        status.code(),
        Some(EXPECTED),
        "dlfcn.h through the loader: 91 dlopen, 92 dlsym of a function, 93 of \
         a datum, 94 dladdr, 95 dlerror, 96 dl_iterate_phdr, 97 a second \
         dlopen, 98 a TLS library's variables at their image's values, 86 and 87 dladdr1's link map and symbol, \
         88 to 90 dlinfo's link map, origin and refusal, 99 dlclose, and a \
         signal a fault"
    );
}

/// The C runtime receives and registers the loader's `rtld_fini` callback.
///
/// The shared library's destructor writes after `main` returns. That is only
/// observable if `crt1.o` passes `rdx` to `__libc_start_main`, the runtime
/// registers it with `atexit`, and the loader reads and calls its
/// `DT_FINI_ARRAY` and then `DT_FINI` entry.
#[test]
fn a_runtime_runs_a_dependency_finalisers_at_exit() {
    let loader = loader();
    let dir = scratch().join("ld-fini");
    std::fs::create_dir_all(&dir).expect("make the scratch directory");
    let (crt, runtime) = runtime(&dir);

    let library = dir.join("libfini.so");
    compile(&[
        "-shared",
        "-Wl,-fini,loader_fini",
        "-o",
        library.to_str().expect("a path"),
        manifest().join("tests/c/fini.c").to_str().expect("a path"),
        manifest()
            .join("tests/c/fini_func.c")
            .to_str()
            .expect("a path"),
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
    assert_eq!(output.stdout, b"main\narray\nfini\n");
}

/// A library's constructor asks for the auxiliary vector before the
/// program's `__libc_start_main` has recorded it, as compiler-builtins' choice
/// of AArch64's LSE atomics does when ferrousli is `libc.so.6`.
///
/// `getauxval` must answer it from the loader's copy (interface revision 3):
/// the library's page size is required to be the one `main` reads. The
/// program exports its C library so that the library's `getauxval` is that
/// one. Exit 1 is no answer even in `main`; 2 is the constructor's wrong one.
#[test]
fn a_library_constructor_reads_the_auxiliary_vector_before_the_program_starts() {
    let loader = loader();
    let dir = scratch().join("ld-early");
    std::fs::create_dir_all(&dir).expect("make the scratch directory");
    let (crt, runtime) = runtime(&dir);

    let library = dir.join("libearly.so");
    compile(&[
        "-shared",
        "-o",
        library.to_str().expect("a path"),
        manifest().join("tests/c/early.c").to_str().expect("a path"),
    ]);

    let program = dir.join("prog");
    let status = Command::new("cc")
        .args([
            "-nostdlib",
            "-fPIC",
            "-O1",
            "-pie",
            "-Wl,--export-dynamic",
            "-o",
        ])
        .arg(&program)
        .arg(&crt)
        .arg(manifest().join("tests/c/early_prog.c"))
        .arg(&runtime)
        .arg(&library)
        .arg(format!("-Wl,--dynamic-linker={}", loader.display()))
        .arg("-Wl,-e,_start")
        .status()
        .expect("build the dynamically linked C program");
    assert!(status.success(), "cc could not build the early fixture");

    let output = Command::new(&program).output().expect("run the program");
    assert_eq!(output.status.code(), Some(EXPECTED));
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
