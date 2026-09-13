//! Driving `cargo` for the two freestanding crates.
//!
//! The loader and the kernel are built for different targets from the same
//! workspace, which is why neither can be a default member and why `cargo
//! build` at the root builds only this tool.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::{self, Arch};
use crate::{Error, Result};

/// Build the UEFI loader for `arch` and return the `.efi` firmware will run.
///
/// On the 64-bit pair that is what rustc produced. On ARMv7-A rustc produces
/// an ELF static PIE, and the `.efi` is written beside it by `pe::convert` —
/// which checks the ELF against `boot/linker/armv7a.ld`'s contract, so a
/// loader that breaks it fails the build rather than the boot.
pub(crate) fn build_loader(arch: Arch, release: bool) -> Result<PathBuf> {
    build("ferrix-boot", arch.loader_target(), release, &[])?;
    if !arch.loader_is_elf() {
        return artifact(arch.loader_target(), release, "ferrix-boot.efi");
    }

    let elf = artifact(arch.loader_target(), release, "ferrix-boot")?;
    let bytes = std::fs::read(&elf)
        .map_err(|error| Error::new(format!("reading {}: {error}", elf.display())))?;
    crate::pe::check_switch(&bytes)?;
    let image = crate::pe::convert(&bytes)?;
    let efi = elf.with_extension("efi");
    std::fs::write(&efi, &image)?;
    println!("  converted the loader to PE32, {} KiB", image.len() / 1024);
    Ok(efi)
}

/// Build the kernel for `arch` and return the ELF it produced.
pub(crate) fn build_kernel(arch: Arch, release: bool) -> Result<PathBuf> {
    build("ferrix-kernel", arch.kernel_target(), release, &[])?;
    artifact(arch.kernel_target(), release, "ferrix-kernel")
}

/// Build `binary`, a native program in `package`, for `arch` into
/// `target_dir`, and return the ELF.
///
/// For the kernel's own target, which is what the kernel's ELF loader takes:
/// freestanding, soft float, and statically relocated by `.cargo/config.toml`.
/// The target directory is a parameter so that a test can build into one of
/// its own and never wait on the lock of the build running it.
pub(crate) fn build_native(
    arch: Arch,
    release: bool,
    package: &str,
    binary: &str,
    target_dir: &Path,
) -> Result<PathBuf> {
    build(
        package,
        arch.kernel_target(),
        release,
        &[("CARGO_TARGET_DIR", target_dir.as_os_str())],
    )?;
    let profile = if release { "release" } else { "debug" };
    let path = target_dir
        .join(arch.kernel_target())
        .join(profile)
        .join(binary);
    if !path.is_file() {
        return Err(Error::new(format!(
            "cargo reported success but {} does not exist",
            path.display()
        )));
    }
    Ok(path)
}

/// Compile the kernel with `init` built in, told to run `script` with `sh -c`.
///
/// Set on the child rather than taken from this process's environment, so the
/// command a person typed is the whole of what was built: a `FERRIX_INIT` left
/// exported in the shell cannot quietly substitute a different program.
pub(crate) fn build_kernel_with_init(
    arch: Arch,
    release: bool,
    init: &Path,
    script: &str,
) -> Result<PathBuf> {
    build(
        "ferrix-kernel",
        arch.kernel_target(),
        release,
        &[
            ("FERRIX_INIT", init.as_os_str()),
            ("FERRIX_INIT_SCRIPT", OsStr::new(script)),
        ],
    )?;
    artifact(arch.kernel_target(), release, "ferrix-kernel")
}

/// Compile the kernel told to run the commands in `commands`, a file
/// `vfs::encode` wrote, in place of a shell.
///
/// Named by path rather than carried in the variable, for the reason
/// `kernel/build.rs` gives: the list is full of NULs.
pub(crate) fn build_kernel_with_commands(
    arch: Arch,
    release: bool,
    commands: &Path,
) -> Result<PathBuf> {
    build(
        "ferrix-kernel",
        arch.kernel_target(),
        release,
        &[("FERRIX_INIT_COMMANDS", commands.as_os_str())],
    )?;
    artifact(arch.kernel_target(), release, "ferrix-kernel")
}

/// Run `cargo build -p <package> --target <target>`, with `env` added.
fn build(package: &str, target: &str, release: bool, env: &[(&str, &OsStr)]) -> Result<()> {
    refuse_inherited_rustflags()?;
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
    for (key, value) in env {
        let _ = command.env(key, value);
    }

    run(command, &format!("cargo build -p {package}"))
}

/// Refuse to build with `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS` set.
///
/// Either one *replaces* the per-target `rustflags` in `.cargo/config.toml`
/// rather than adding to them, and those flags are what put the kernel where the
/// loader maps it: the linker script, the page size, the static relocation
/// model. Without them the build still succeeds and the image is still written,
/// and the failure arrives at boot as a loader panic that names neither the
/// variable nor the cause. CI set `RUSTFLAGS: -D warnings` for its whole
/// workflow and lost its first boot test exactly that way.
fn refuse_inherited_rustflags() -> Result<()> {
    for variable in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        if std::env::var_os(variable).is_some_and(|value| !value.is_empty()) {
            return Err(Error::new(format!(
                "{variable} is set. {}",
                concat!(
                    "It replaces the per-target rustflags in .cargo/config.toml rather ",
                    "than adding to them, which drops the kernel's linker script: the ",
                    "image would build and then fail to boot. Unset it, and deny ",
                    "warnings with `cargo clippy -- -D warnings` instead.",
                ),
            )));
        }
    }
    Ok(())
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
