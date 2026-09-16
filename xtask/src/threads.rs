//! `test-threads`: stage 7's threads exit test.
//!
//! "A static musl Rust program that uses `std::thread`, `Mutex` and `mpsc`
//! runs under `test-shell` on all three architectures." The program is
//! `threads-test/`, built here for each architecture's musl target and booted
//! as init. It prints a line per step and `threads: all ok`, and exits 0; this
//! requires every one of those lines, in order, and the status.
//!
//! Then the negative control: the same program built to expect one thread more
//! under `/proc/self` than it has must fail, on the `/proc` step and for that
//! reason, and still end with the status it chose. A check of the thread count
//! that could not fail would pass that build too.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::{Error, Result, cargo, fat, native, qemu, shell};

/// The Rust target the program is built for on `arch`.
fn target(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64-unknown-linux-musl",
        Arch::AArch64 => "aarch64-unknown-linux-musl",
        Arch::Armv7a => "armv7-unknown-linux-musleabi",
    }
}

/// The lines a working run prints, in order.
const STEPS: &[&str] = &[
    "threads: spawn ok",
    "threads: proc ok",
    "threads: channel ok",
    "threads: join ok",
    "threads: mutex ok",
    "threads: all ok",
];

/// The line the negative control must fail with.
const NEGATIVE_FAILURE: &str = "threads: FAILED /proc/self/status does not count every thread";

/// Build `threads-test` for `arch`, as the negative control or not, and return
/// where the program is.
fn build(arch: Arch, negative: bool) -> Result<PathBuf> {
    let target = target(arch);
    let flavour = if negative { "negative" } else { "plain" };
    let target_dir = paths::target_dir().join("threads-test").join(flavour);
    println!("  building threads-test ({flavour}) for {target}");
    let mut command = Command::new(cargo::cargo());
    let _ = command
        .current_dir(paths::workspace_root().join("threads-test"))
        .args(["build", "--release", "--target", target])
        .env("CARGO_TARGET_DIR", &target_dir);
    if negative {
        let _ = command.args(["--features", "negative-control"]);
    }
    // Ferrix's own `.cargo/config.toml` uses this triple for the ARMv7-A
    // loader, with its linker script, and cargo merges a parent directory's
    // flags for a target into `threads-test/`'s. The variable replaces every
    // configured flag, so the program is linked as the other two are.
    if arch == Arch::Armv7a {
        let _ = command
            .env(
                "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABI_LINKER",
                "rust-lld",
            )
            .env(
                "RUSTFLAGS",
                "-C linker-flavor=ld.lld -C link-self-contained=yes \
                 -C target-feature=+crt-static",
            );
    }
    cargo::run(command, "cargo build (threads-test)")?;
    Ok(target_dir.join(target).join("release").join("threads-test"))
}

/// Boot `program` as init on `arch` and return every line after the boot
/// marker, once init has exited.
fn boot(arch: Arch, program: &Path, args: &Args) -> Result<Vec<String>> {
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, program, shell::SCRIPT)?;
    let natives = native::build(arch, args.release)?;
    let image = fat::write_image(arch, &loader, &kernel, &natives, None)?;
    let lines = qemu::watch_then(arch, &image, &kernel, args, shell::EXITED, |_| Ok(()))?;
    let after_boot = lines
        .iter()
        .position(|line| line.contains(qemu::SUCCESS_MARKER))
        .and_then(|at| lines.get(at..))
        .unwrap_or_default()
        .to_vec();
    Ok(after_boot)
}

/// The status init exited with, from the kernel's line.
fn status(lines: &[String]) -> Option<i32> {
    lines.iter().find_map(|line| {
        let at = line.find(shell::EXITED)?;
        line.get(at + shell::EXITED.len()..)?.trim().parse().ok()
    })
}

/// Whether `line` is `want`, once the serial log's time stamp is taken off.
fn says(line: &str, want: &str) -> bool {
    line.trim_end().ends_with(want)
}

/// The test on every architecture asked for.
pub(crate) fn test_threads(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        let log = paths::build_dir(arch).join("serial.log");

        let program = build(arch, false)?;
        let lines = boot(arch, &program, args)?;
        let mut remaining = lines.iter();
        for want in STEPS {
            if !remaining.any(|line| says(line, want)) {
                let failed = lines.iter().find(|line| line.contains("threads: FAILED"));
                return Err(Error::new(format!(
                    "{arch}: the threads test did not print `{want}` in order{}.\n  \
                     Serial output is in {}",
                    failed.map_or(String::new(), |line| format!("; it said `{}`", line.trim())),
                    log.display()
                )));
            }
        }
        if status(&lines) != Some(0) {
            return Err(Error::new(format!(
                "{arch}: the threads test did not exit with 0.\n  Serial output is in {}",
                log.display()
            )));
        }
        println!(
            "  {arch}: std::thread, Mutex and mpsc ran, /proc/self counted every thread, exit 0"
        );

        let negative = build(arch, true)?;
        let lines = boot(arch, &negative, args)?;
        let failed_there = lines.iter().any(|line| says(line, NEGATIVE_FAILURE));
        let passed_proc = lines.iter().any(|line| says(line, "threads: proc ok"));
        if !failed_there || passed_proc || status(&lines) != Some(1) {
            return Err(Error::new(format!(
                "{arch}: the negative control should fail with `{NEGATIVE_FAILURE}` and exit 1, \
                 and did not.\n  Serial output is in {}",
                log.display()
            )));
        }
        println!("  {arch}: the negative control failed on the /proc thread count, as it must");
    }
    Ok(())
}
