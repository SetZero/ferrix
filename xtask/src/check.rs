//! `cargo xtask check` — every gate CI runs, run locally in one command.
//!
//! The point is that a contributor never learns from CI something they could
//! have learned in a minute. The order is cheapest-first, so the gate most
//! likely to fail on a work-in-progress tree fails first.

use std::process::{Command, Stdio};

use crate::args::Args;
use crate::cargo::{self, cargo as cargo_binary};
use crate::paths::{self, Arch};
use crate::{Error, Result};

/// Run the gate set.
pub(crate) fn run(args: &Args) -> Result<()> {
    let root = paths::workspace_root();

    // First because it is the cheapest, and because it is the gate that says
    // whether the *other* local gates -- the commit hooks -- are running at
    // all. They are files until `core.hooksPath` points at them, and a clone
    // where nobody ran that line refuses nothing.
    step("commit hooks", || {
        python_with("scripts/check-commit-authors.py", &["--hooks"])
    })?;

    step("formatting", || {
        let mut command = Command::new(cargo_binary());
        let _ = command
            .current_dir(&root)
            .args(["fmt", "--all", "--", "--check"]);
        cargo::run(command, "cargo fmt --check")
    })?;

    step("line endings", || python("scripts/check-line-endings.py"))?;

    step("assembly allow-list", || {
        python("scripts/check-asm-budget.py")
    })?;

    step("unsafe audit", || python("scripts/check-unsafe-audit.py"))?;
    step("panic audit", || python("scripts/check-panic-audit.py"))?;

    // The architecture document is generated from `docs/sysml/` and committed.
    // A model edited without regenerating leaves the two disagreeing, and the
    // document is exactly where nobody would notice; this is the cheapest
    // possible place to say so.
    step("architecture document", || {
        python("scripts/sysml/tests.py")?;
        python_with("scripts/gen-arch-doc.py", &["--check"])
    })?;

    // The panic screen's font is generated from the BDF committed beside it,
    // and a hand edit to either would otherwise drift silently.
    step("font", || python_with("scripts/gen-font.py", &["--check"]))?;

    // The explanations a panic prints are rendered into a document, which
    // goes stale the moment an entry changes without it.
    step("panic catalog", || {
        python_with("scripts/gen-panic-catalog.py", &["--check"])
    })?;

    step("crate layering", || {
        let mut command = Command::new("bash");
        let _ = command
            .current_dir(&root)
            .arg("scripts/check-crate-layering.sh");
        cargo::run(command, "scripts/check-crate-layering.sh")
    })?;

    // The runtime and the native programs are freestanding like the kernel:
    // a `_start`, a panic handler, a linker script. The host can build none of
    // them, so they are linted per target below, and what of them can be
    // tested lives in `libs/native`.
    step("clippy (host)", || {
        let mut arguments = vec!["--workspace"];
        arguments.extend(
            FREESTANDING
                .iter()
                .flat_map(|&package| ["--exclude", package]),
        );
        arguments.push("--all-targets");
        clippy(&arguments)
    })?;

    step("tests", || {
        let mut command = Command::new(cargo_binary());
        let _ = command
            .current_dir(&root)
            .args(["test", "--workspace"])
            .args(
                FREESTANDING
                    .iter()
                    .flat_map(|&package| ["--exclude", package]),
            )
            .arg("--all-targets");
        cargo::run(command, "cargo test")
    })?;

    if args.ferrousli {
        ferrousli(&root)?;
    }

    // Opt-in, because it is minutes rather than seconds and needs a nightly
    // toolchain with the miri component: the same crates, in the same order,
    // as CI's Miri job, so a UB report can be reproduced before pushing.
    if args.miri {
        for package in MIRI_PACKAGES {
            step(&format!("miri ({package})"), || miri(package))?;
        }
    }

    if args.fast {
        println!("\nchecked (--fast: cross-target clippy skipped)");
        return Ok(());
    }

    // The freestanding halves, once per target. A lint pass for x86-64 cannot
    // see Arm code at all, so skipping these means two thirds of the kernel go
    // unlinted until CI.
    for arch in Arch::ALL {
        step(&format!("clippy (kernel, {arch})"), || {
            clippy(&["-p", "ferrix-kernel", "--target", arch.kernel_target()])
        })?;
        step(&format!("clippy (loader, {arch})"), || {
            clippy(&["-p", "ferrix-boot", "--target", arch.loader_target()])
        })?;
        step(
            &format!("clippy (native runtime and programs, {arch})"),
            || {
                let mut arguments: Vec<&str> =
                    NATIVE.iter().flat_map(|&package| ["-p", package]).collect();
                arguments.extend(["--target", arch.kernel_target()]);
                clippy(&arguments)
            },
        )?;
    }

    println!("\nchecked");
    Ok(())
}

/// ferrousli's gates, behind `--ferrousli`.
///
/// ferrousli is a workspace of its own, so none of the steps above reach it,
/// and a change elsewhere could break it without a gate noticing. These are
/// the gates its landings have always run. The tests run twice because each C
/// program is built at `-O0` and `-O2` in both profiles, and the library
/// itself behaves differently optimised: a release build is where the
/// optimiser turns loops into calls to the library's own `memcpy`.
///
/// Off by default: building the library and its C programs twice is minutes,
/// and most landings cannot affect it. `docs/BACKLOG.md` says which landings
/// must pass it.
fn ferrousli(root: &std::path::Path) -> Result<()> {
    let dir = root.join("ferrousli");
    let in_ferrousli = |arguments: &[&str]| {
        let mut command = Command::new(cargo_binary());
        let _ = command.current_dir(&dir).args(arguments);
        command
    };

    step("ferrousli: generated ABI", || {
        python_with("ferrousli/tools/gen-abi.py", &["--check"])
    })?;
    step("ferrousli: formatting", || {
        cargo::run(in_ferrousli(&["fmt", "--check"]), "cargo fmt (ferrousli)")
    })?;
    step("ferrousli: clippy", || {
        cargo::run(
            in_ferrousli(&["clippy", "--all-targets", "--", "-D", "warnings"]),
            "cargo clippy (ferrousli)",
        )
    })?;
    step("ferrousli: tests", || {
        cargo::run(in_ferrousli(&["test"]), "cargo test (ferrousli)")
    })?;
    step("ferrousli: tests (release)", || {
        cargo::run(
            in_ferrousli(&["test", "--release"]),
            "cargo test --release (ferrousli)",
        )
    })
}

/// `cargo xtask model-doc` -- regenerate the document the gate above checks.
pub(crate) fn model_doc() -> Result<()> {
    python_with("scripts/gen-arch-doc.py", &[])
}

/// The native runtime and the programs built on it: freestanding, so linted
/// once per kernel target rather than for the host.
const NATIVE: &[&str] = &[
    "ferrix-rt",
    "ferrix-channel-echo",
    "ferrix-blk",
    "ferrix-devmgr",
];

/// Every workspace member the host cannot build: the loader, the kernel, and
/// [`NATIVE`].
const FREESTANDING: &[&str] = &[
    "ferrix-kernel",
    "ferrix-boot",
    "ferrix-rt",
    "ferrix-channel-echo",
    "ferrix-blk",
    "ferrix-devmgr",
];

/// The crates CI's Miri job interprets, in its order.
///
/// A test below reads `.github/workflows/ci.yml` and fails when the two
/// disagree, so a step added to one and not the other is found by `cargo
/// xtask check` rather than by a contributor who trusted `--miri`.
const MIRI_PACKAGES: [&str; 13] = [
    "ferrix-elf",
    "ferrix-bootinfo",
    "ferrix-ustack",
    "ferrix-objects",
    "ferrix-vfs",
    "ferrix-pci",
    "ferrix-block",
    "ferrix-blkring",
    "ferrix-native",
    "ferrix-virtio-blk",
    "ferrix-frame",
    "ferrix-heap",
    "ferrix-paging",
];

/// `cargo +nightly miri test -p <package> --lib`.
///
/// Through the rustup proxy by name rather than [`cargo_binary`]: `CARGO` is
/// the pinned toolchain's own cargo, which does not understand `+nightly`. The
/// variables the outer cargo exported would otherwise pin the inner one back
/// to that toolchain.
fn miri(package: &str) -> Result<()> {
    let mut command = Command::new("cargo");
    let _ = command
        .current_dir(paths::workspace_root())
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO")
        .env_remove("RUSTC")
        .env_remove("RUSTDOC")
        .args(["+nightly", "miri", "test", "-p", package, "--lib"]);
    cargo::run(command, "cargo +nightly miri test")
}

/// Announce a gate, run it, and report.
fn step(name: &str, body: impl FnOnce() -> Result<()>) -> Result<()> {
    println!("\n== {name}");
    body()
}

/// `cargo clippy ... -- -D warnings`.
fn clippy(arguments: &[&str]) -> Result<()> {
    let mut command = Command::new(cargo_binary());
    let _ = command
        .current_dir(paths::workspace_root())
        .arg("clippy")
        .args(arguments)
        .args(["--", "-D", "warnings"]);
    cargo::run(command, "cargo clippy")
}

/// Run one of the gate scripts.
///
/// The interpreter is *probed*, not guessed. `python3` exists on a stock
/// Windows install as an App Execution Alias that is not Python at all: it
/// prints an advertisement for the Microsoft Store and exits 9009. Looking the
/// name up on PATH finds it, so the only reliable test is to run it.
fn python(script: &str) -> Result<()> {
    python_with(script, &[])
}

/// Run one of the gate scripts, passing it arguments.
fn python_with(script: &str, arguments: &[&str]) -> Result<()> {
    let interpreter = python_interpreter().ok_or_else(|| {
        Error::new("no working Python interpreter on PATH (tried python3, python)")
    })?;

    let mut command = Command::new(interpreter);
    let _ = command
        .current_dir(paths::workspace_root())
        .arg(script)
        .args(arguments);
    cargo::run(command, script)
}

/// The first name on PATH that answers `--version` like an interpreter.
fn python_interpreter() -> Option<&'static str> {
    ["python3", "python", "py"].into_iter().find(|name| {
        Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miri_runs_the_crates_ci_interprets_in_the_same_order() {
        let workflow =
            std::fs::read_to_string(paths::workspace_root().join(".github/workflows/ci.yml"))
                .unwrap();
        let in_ci: Vec<&str> = workflow
            .lines()
            .filter_map(|line| {
                let rest = line
                    .trim()
                    .strip_prefix("run: cargo +nightly miri test -p ")?;
                rest.split_whitespace().next()
            })
            .collect();
        assert_eq!(
            in_ci, MIRI_PACKAGES,
            "`cargo xtask check --miri` and CI's Miri job must name the same \
             crates; change MIRI_PACKAGES in xtask/src/check.rs with the workflow"
        );
    }
}
