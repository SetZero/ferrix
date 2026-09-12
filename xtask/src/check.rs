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

    step("clippy (host)", || {
        clippy(&[
            "--workspace",
            "--exclude",
            "ferrix-kernel",
            "--exclude",
            "ferrix-boot",
            "--all-targets",
        ])
    })?;

    step("tests", || {
        let mut command = Command::new(cargo_binary());
        let _ = command.current_dir(&root).args([
            "test",
            "--workspace",
            "--exclude",
            "ferrix-kernel",
            "--exclude",
            "ferrix-boot",
            "--all-targets",
        ]);
        cargo::run(command, "cargo test")
    })?;

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
    }

    println!("\nchecked");
    Ok(())
}

/// `cargo xtask model-doc` -- regenerate the document the gate above checks.
pub(crate) fn model_doc() -> Result<()> {
    python_with("scripts/gen-arch-doc.py", &[])
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
