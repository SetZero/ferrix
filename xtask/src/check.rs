//! `cargo xtask check` — every gate CI runs, run locally in one command.
//!
//! The point is that a contributor never learns from CI something they could
//! have learned in a minute. The order is cheapest-first, so the gate most
//! likely to fail on a work-in-progress tree fails first.

use std::process::{Command, Stdio};

use crate::args::Args;
use crate::cargo::{self, cargo as cargo_binary};
use crate::paths::{self, Arch};
use crate::workspace;
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

    // The seam §7 is built on: the kernel enumerates devices and drives none.
    // A convenient register access in the wrong file is how that claim decays,
    // and it decays silently, so it is asserted here rather than reviewed for.
    step("device-access allow-list", || {
        python("scripts/check-device-access.py")
    })?;

    step("unsafe audit", || python("scripts/check-unsafe-audit.py"))?;
    step("panic audit", || python("scripts/check-panic-audit.py"))?;

    // The boundary four assurance ratings attach to. Every artifact in
    // docs/certification is scoped to `scripts/certification-item.json`, so a
    // kernel file that drifts into the trusted core -- or an unclassified new
    // one that nobody decided about -- silently changes what those ratings
    // claim. docs/certification/ITEM.md.
    step("certification item boundary", || {
        python("scripts/check-item-boundary.py")
    })?;

    // The item links no external crate on any architecture, which is what lets
    // IEC 62304's SOUP obligation be answered with "none" rather than with an
    // anomaly-list evaluation per dependency. That is a property worth
    // re-establishing rather than remembering. docs/certification/SOUP.md.
    step("SOUP register", || {
        python_with("scripts/gen-soup.py", &["--check"])
    })?;

    // The architecture document is generated from `docs/sysml/` and committed.
    // A model edited without regenerating leaves the two disagreeing, and the
    // document is exactly where nobody would notice; this is the cheapest
    // possible place to say so.
    step("architecture document", || {
        python("scripts/sysml/tests.py")?;
        python_with("scripts/gen-arch-doc.py", &["--check"])
    })?;

    // The compositor's interface tables are generated from the protocol XML
    // vendored beside them. A table edited by hand is a compositor that reads
    // a client's message with the wrong signature, which is the kind of bug
    // that shows up as one misdrawn window an hour later.
    step("wayland protocol tables", || {
        python_with("scripts/gen-wayland-protocol.py", &["--check"])
    })?;

    // The keymap the compositor hands every client, and the modifier bits
    // that go with it, are libxkbcommon's own output through a committed
    // probe. A keymap edited by hand is a keyboard that types the wrong
    // letters, and the client is the only thing that would notice.
    step("xkb keymap and tables", || {
        python_with("scripts/gen-xkb-tables.py", &["--check"])
    })?;

    // The panic screen's font is generated from the BDF committed beside it,
    // and a hand edit to either would otherwise drift silently.
    step("font", || python_with("scripts/gen-font.py", &["--check"]))?;

    // The terminal's font is rasterised from the TrueType faces committed
    // beside it, by a rasteriser in the repository rather than by whatever
    // FreeType the machine has: that is what makes "byte-identical" a demand
    // this gate can make of every checkout.
    step("terminal font", || {
        python_with("scripts/gen-term-font.py", &["--check"])
    })?;

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
    step("clippy (host)", host_clippy)?;
    step("tests", host_test)?;
    step("doc tests", host_doctest)?;
    step("documentation", host_doc)?;

    compositor(&root)?;

    if args.ferrousli {
        ferrousli(&root)?;
    }

    if args.zinc {
        zinc(&root)?;
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
            || native_clippy(arch),
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
///
/// On Windows every cargo step runs in WSL, for the reason `crate::wsl`
/// gives: the tests start Linux executables. The generated-ABI check reads
/// headers in the tree and runs natively.
fn ferrousli(root: &std::path::Path) -> Result<()> {
    let dir = root.join("ferrousli");
    let in_ferrousli = |arguments: &[&str]| {
        if cfg!(windows) {
            return crate::wsl::cargo(&dir, arguments);
        }
        let mut command = Command::new(cargo_binary());
        let _ = command.current_dir(&dir).args(arguments);
        command
    };
    if cfg!(windows) {
        step("ferrousli: WSL", || {
            crate::wsl::require_toolchain("ferrousli's tests run C programs built for Linux")
        })?;
    }

    step("ferrousli: generated ABI", || {
        python_with("ferrousli/tools/gen-abi.py", &["--check"])
    })?;
    step("ferrousli: formatting", || {
        cargo::run(in_ferrousli(&["fmt", "--check"]), "cargo fmt (ferrousli)")
    })?;
    // `--workspace`, because ferrousli's root is a package as well as a
    // workspace, and cargo run there without it covers that package alone:
    // the loader in `ld/` went ungated until 2026-09-21 for that reason.
    step("ferrousli: clippy", || {
        cargo::run(
            in_ferrousli(&[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]),
            "cargo clippy (ferrousli)",
        )
    })?;
    // The library on AArch64 and ARMv7-A, whose code the host's build never
    // compiles. Clippy needs only their rustup targets; the tests there need
    // a cross compiler and QEMU's user mode, which `ferrousli/README.md` says
    // how to run.
    for target in ["aarch64-unknown-linux-gnu", "armv7-unknown-linux-gnueabihf"] {
        step(&format!("ferrousli: clippy ({target})"), || {
            cargo::run(
                in_ferrousli(&[
                    "clippy", "--lib", "--target", target, "--", "-D", "warnings",
                ]),
                "cargo clippy (ferrousli)",
            )
        })?;
    }
    // The loader, which `--all-targets` skips: its binary is built only with
    // the `loader` feature and for a musl target (`ld/Cargo.toml`), and it
    // went unlinted until 2026-09-23 for that reason.
    for target in [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "armv7-unknown-linux-musleabihf",
    ] {
        step(&format!("ferrousli: clippy (loader, {target})"), || {
            cargo::run(
                in_ferrousli(&[
                    "clippy",
                    "-p",
                    "ferrousli-ld",
                    "--bin",
                    "ld-ferrousli",
                    "--features",
                    "loader",
                    "--target",
                    target,
                    "--",
                    "-D",
                    "warnings",
                ]),
                "cargo clippy (ferrousli's loader)",
            )
        })?;
    }
    step("ferrousli: tests", || {
        cargo::run(
            in_ferrousli(&["test", "--workspace"]),
            "cargo test (ferrousli)",
        )
    })?;
    step("ferrousli: tests (release)", || {
        cargo::run(
            in_ferrousli(&["test", "--workspace", "--release"]),
            "cargo test --release (ferrousli)",
        )
    })
}

/// zinc's gates, behind `--zinc`.
///
/// zinc is a workspace of its own too, and `build` only compiles it into the
/// initramfs. These are the gates a zinc landing runs: formatting, clippy,
/// the unit tests, and two gates driven through a pseudo-terminal: the line
/// editor's completion, which is the only way to see what a Tab does, and
/// job control, which is the only way to see a process group at all.
///
/// Clippy and the tests build with the `next` feature, so `zinc-next`, the
/// port of zsh's runtime that lands in slices, meets the same gate as
/// `zinc`. Clippy allows `excessive_nesting`: the runtime zinc landed with
/// carries that warning in its parser and builtins, which that port replaces
/// rather than restructures. Every other warning fails the gate.
///
/// The tests start Linux executables and the pty needs a Linux kernel, so on
/// Windows those steps run in WSL, as ferrousli's do.
fn zinc(root: &std::path::Path) -> Result<()> {
    let dir = root.join("zinc");
    const TARGET: &str = "x86_64-unknown-linux-musl";
    let native = |arguments: &[&str]| {
        let mut command = Command::new(cargo_binary());
        let _ = command.current_dir(&dir).args(arguments);
        command
    };
    step("zinc: formatting", || {
        cargo::run(native(&["fmt", "--check"]), "cargo fmt (zinc)")
    })?;
    step("zinc: clippy", || {
        cargo::run(
            native(&[
                "clippy",
                "--target",
                TARGET,
                "--all-targets",
                "--features",
                "next",
                "--",
                "-D",
                "warnings",
                "-A",
                "clippy::excessive_nesting",
            ]),
            "cargo clippy (zinc)",
        )
    })?;
    if cfg!(windows) {
        step("zinc: WSL", || {
            crate::wsl::require_toolchain("zinc's tests drive a pseudoterminal")
        })?;
    }
    step("zinc: tests", || {
        let command = if cfg!(windows) {
            crate::wsl::cargo(&dir, &["test", "--features", "next", "--target", TARGET])
        } else {
            native(&["test", "--features", "next", "--target", TARGET])
        };
        cargo::run(command, "cargo test (zinc)")
    })?;
    step("zinc: completion on a pty", || {
        let script = "cargo build --release --target \"$1\" && \
                      python3 tests/pty_completion.py \"$CARGO_TARGET_DIR/$1/release/zinc\"";
        let command = if cfg!(windows) {
            crate::wsl::bash(&dir, script, &[TARGET])
        } else {
            let mut command = Command::new("bash");
            let _ = command
                .current_dir(&dir)
                .env("CARGO_TARGET_DIR", paths::target_dir().join("zinc-check"))
                .args(["-c", script, "bash", TARGET]);
            command
        };
        cargo::run(command, "zinc/tests/pty_completion.py")
    })?;
    step("zinc: job control on a pty", || {
        let script = "cargo build --release --target \"$1\" && \
                      python3 tests/pty_jobs.py \"$CARGO_TARGET_DIR/$1/release/zinc\"";
        let command = if cfg!(windows) {
            crate::wsl::bash(&dir, script, &[TARGET])
        } else {
            let mut command = Command::new("bash");
            let _ = command
                .current_dir(&dir)
                .env("CARGO_TARGET_DIR", paths::target_dir().join("zinc-check"))
                .args(["-c", script, "bash", TARGET]);
            command
        };
        cargo::run(command, "zinc/tests/pty_jobs.py")
    })
}

/// The compositor's gates.
///
/// `compositor/` is a workspace of its own, like ferrousli, so the steps
/// above never reach it. They are on by default, because they are seconds
/// rather than minutes.
///
/// They also go through WSL on Windows now, which the comment here used to
/// say would happen "when a crate needs a Linux host". `compositor/virgl` is
/// that crate: `device.rs` holds an `OwnedFd` for a render node and
/// `vtest.rs` speaks virglrenderer's protocol over a `UnixStream`, neither of
/// which `std` has on Windows, and `drm`, `render` and `hyprix` all build on
/// it. So the host pass stopped compiling on Windows the day the GPU work
/// landed, with rustc's "cannot find `unix` in `os`", and `cargo xtask check`
/// could not finish on that host at all.
///
/// Running it in the distribution rather than excluding the crates keeps the
/// two hosts checking the same code: an exclusion would have left the GPU
/// path linted on Linux and nowhere else, which is the half of the tree most
/// worth linting and the half a Windows developer is most likely to be
/// changing.
fn compositor(root: &std::path::Path) -> Result<()> {
    let dir = root.join("compositor");
    let in_compositor = |arguments: &[&str]| {
        if cfg!(windows) {
            return crate::wsl::cargo(&dir, arguments);
        }
        let mut command = Command::new(cargo_binary());
        let _ = command.current_dir(&dir).args(arguments);
        command
    };
    if cfg!(windows) {
        step("compositor: WSL", || {
            crate::wsl::require_toolchain("the compositor opens render nodes and Unix sockets")
        })?;
    }
    step("compositor: formatting", || {
        cargo::run(in_compositor(&["fmt", "--check"]), "cargo fmt (compositor)")
    })?;
    step("compositor: clippy", || {
        cargo::run(
            in_compositor(&["clippy", "--all-targets", "--", "-D", "warnings"]),
            "cargo clippy (compositor)",
        )
    })?;
    step("compositor: tests", || {
        cargo::run(in_compositor(&["test"]), "cargo test (compositor)")
    })
}

/// `cargo xtask model-doc` -- regenerate the document the gate above checks.
pub(crate) fn model_doc() -> Result<()> {
    python_with("scripts/gen-arch-doc.py", &[])
}

// The host half of the gate, one function per CI step. `cargo xtask check`
// runs them and CI calls them by name (`cargo xtask host-test` and the rest),
// so the two cannot disagree about which members the host builds: both ask
// `workspace::members`.

/// The workspace's members, sorted.
fn sorted_members() -> Result<workspace::Members> {
    workspace::members(&paths::workspace_root())
}

/// `cargo clippy` over every host member, every target: `cargo xtask host-clippy`.
pub(crate) fn host_clippy() -> Result<()> {
    let excludes = sorted_members()?.excludes();
    let mut arguments: Vec<&str> = vec!["--workspace"];
    arguments.extend(excludes.iter().map(String::as_str));
    arguments.push("--all-targets");
    clippy(&arguments)
}

/// `cargo test` over every host member, every target: `cargo xtask host-test`.
pub(crate) fn host_test() -> Result<()> {
    host_cargo(&["test"], &["--all-targets"], &[], "cargo test")
}

/// The doc tests `--all-targets` skips: `cargo xtask host-doctest`.
pub(crate) fn host_doctest() -> Result<()> {
    host_cargo(&["test"], &["--doc"], &[], "cargo test --doc")
}

/// The documentation, warnings denied, as the lint config denies broken
/// intra-doc links: `cargo xtask host-doc`.
pub(crate) fn host_doc() -> Result<()> {
    host_cargo(
        &["doc"],
        &["--no-deps"],
        &[("RUSTDOCFLAGS", "-D warnings")],
        "cargo doc",
    )
}

/// `cargo clippy` over the native runtime and programs for `arch`'s kernel
/// target: `cargo xtask native-clippy --arch ARCH`.
pub(crate) fn native_clippy(arch: Arch) -> Result<()> {
    let native = sorted_members()?.native;
    let mut arguments: Vec<&str> = native
        .iter()
        .flat_map(|package| ["-p", package.as_str()])
        .collect();
    arguments.extend(["--target", arch.kernel_target()]);
    clippy(&arguments)
}

/// `cargo <command> --workspace` without the freestanding members, then
/// `extra`, with `environment` set.
fn host_cargo(
    command: &[&str],
    extra: &[&str],
    environment: &[(&str, &str)],
    what: &str,
) -> Result<()> {
    let excludes = sorted_members()?.excludes();
    let mut process = Command::new(cargo_binary());
    let _ = process
        .current_dir(paths::workspace_root())
        .args(command)
        .arg("--workspace")
        .args(&excludes)
        .args(extra)
        .envs(environment.iter().copied());
    cargo::run(process, what)
}

/// The crates CI's Miri job interprets, in its order.
///
/// A test below reads `.github/workflows/ci.yml` and fails when the two
/// disagree, so a step added to one and not the other is found by `cargo
/// xtask check` rather than by a contributor who trusted `--miri`.
const MIRI_PACKAGES: [&str; 14] = [
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
    "ferrix-svc",
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
