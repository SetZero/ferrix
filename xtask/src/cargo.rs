//! Driving `cargo` for the two freestanding crates.
//!
//! The loader and the kernel are built for different targets from the same
//! workspace, which is why neither can be a default member and why `cargo
//! build` at the root builds only this tool.

use std::path::PathBuf;
use std::process::Command;

use crate::paths::{self, Arch};
use crate::{Error, Result};

/// Build the UEFI loader for `arch` and return the `.efi` it produced.
pub(crate) fn build_loader(arch: Arch, release: bool) -> Result<PathBuf> {
    build("ferrix-boot", arch.loader_target(), release)?;
    artifact(arch.loader_target(), release, "ferrix-boot.efi")
}

/// Build the kernel for `arch` and return the ELF it produced.
pub(crate) fn build_kernel(arch: Arch, release: bool) -> Result<PathBuf> {
    build("ferrix-kernel", arch.kernel_target(), release)?;
    artifact(arch.kernel_target(), release, "ferrix-kernel")
}

/// Run `cargo build -p <package> --target <target>`.
fn build(package: &str, target: &str, release: bool) -> Result<()> {
    println!("  building {package} for {target}");

    let mut command = Command::new(cargo());
    let _ = command.current_dir(paths::workspace_root()).args([
        "build",
        "--package",
        package,
        "--target",
        target,
    ]);
    if release {
        let _ = command.arg("--release");
    }

    run(command, &format!("cargo build -p {package}"))
}

/// The path an artefact was written to, checked for existence so that a
/// rename in a manifest fails here rather than as a confusing image error.
fn artifact(target: &str, release: bool, name: &str) -> Result<PathBuf> {
    let profile = if release { "release" } else { "debug" };
    let path = paths::target_dir().join(target).join(profile).join(name);
    if !path.is_file() {
        return Err(Error::new(format!(
            "cargo reported success but {} does not exist",
            path.display()
        )));
    }
    Ok(path)
}

/// Run a command, turning a non-zero status into an error that names it.
pub(crate) fn run(mut command: Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .map_err(|error| Error::new(format!("could not run {description}: {error}")))?;

    if !status.success() {
        return Err(Error::new(format!(
            "{description} failed{}",
            status
                .code()
                .map_or(String::new(), |code| format!(" (exit {code})"))
        )));
    }
    Ok(())
}

/// The cargo to re-enter with, so that a `+toolchain` invocation stays on the
/// toolchain the user chose.
pub(crate) fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}
