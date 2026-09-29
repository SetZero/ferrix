//! Builds `crt1.o`, the object every program links first.
//!
//! `crt1.o` cannot live inside `libferrousli.a`. A linker pulls an archive
//! member in only to satisfy a symbol something already needs, and nothing
//! needs `_start`: it is where execution begins, not a call anyone makes. So,
//! as with every C library, it is a separate object named on the command line.
//! It is compiled here by the same `rustc` that builds the library, so the entry
//! code stays Rust and needs no assembler.

#![allow(
    clippy::expect_used,
    reason = "a build script reports failure by panicking"
)]

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("crt1.o");
    let target = env::var("TARGET").expect("cargo sets TARGET");
    let rustc = env::var_os("RUSTC").expect("cargo sets RUSTC");

    println!("cargo::rerun-if-changed=crt/crt1.rs");
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
        .arg("crt/crt1.rs")
        .status()
        .expect("run rustc");
    assert!(status.success(), "rustc could not build crt/crt1.rs");

    // The C program tests link it; they find it through this.
    println!("cargo::rustc-env=FERROUSLI_CRT1={}", out.display());
}
