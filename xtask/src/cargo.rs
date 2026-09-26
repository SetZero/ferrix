//! Driving `cargo` for the two freestanding crates.
//!
//! The loader and the kernel are built for different targets from the same
//! workspace, which is why neither can be a default member and why `cargo
//! build` at the root builds only this tool.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::args::Mitigations;
use crate::builds::Build;
use crate::paths::{self, Arch};
use crate::{Error, Result};

/// Build the UEFI loader for `arch` and return the `.efi` firmware will run.
///
/// On the 64-bit pair that is what rustc produced. On ARMv7-A rustc produces
/// an ELF static PIE, and the `.efi` is written beside it by `pe::convert` —
/// which checks the ELF against `boot/linker/armv7a.ld`'s contract, so a
/// loader that breaks it fails the build rather than the boot.
pub(crate) fn build_loader(arch: Arch, release: bool) -> Result<PathBuf> {
    let name = if arch.loader_is_elf() {
        "ferrix-boot"
    } else {
        "ferrix-boot.efi"
    };
    let made = output(arch.loader_target(), release, name);
    build("ferrix-boot", arch.loader_target(), release)?
        .output(&made)
        .run()?;
    let made = artifact(made)?;
    if !arch.loader_is_elf() {
        return Ok(made);
    }

    let elf = made;
    let bytes = std::fs::read(&elf)
        .map_err(|error| Error::new(format!("reading {}: {error}", elf.display())))?;
    crate::pe::check_switch(&bytes)?;
    let image = crate::pe::convert(&bytes)?;
    let efi = elf.with_extension("efi");
    std::fs::write(&efi, &image)?;
    println!("  converted the loader to PE32, {} KiB", image.len() / 1024);
    Ok(efi)
}

/// Whether this run builds its kernels with `--mitigations off`.
static MITIGATIONS_OFF: AtomicBool = AtomicBool::new(false);

/// Say how every kernel this run builds is to be built: `main` calls it once,
/// with what `--mitigations` said.
pub(crate) fn set_mitigations(setting: Mitigations) {
    MITIGATIONS_OFF.store(setting == Mitigations::Off, Ordering::Relaxed);
}

/// The `--config` that builds the kernel for `target` without its
/// side-channel defences.
///
/// A `--config` array is *appended* to the one in `.cargo/config.toml`, so the
/// per-target flags [`refuse_inherited_rustflags`] protects are kept and the
/// `cfg` is added -- where `RUSTFLAGS` would have replaced them. It reaches
/// every crate built for the target, the libraries' clamps included
/// (`ferrix_sync::nospec`).
pub(crate) fn mitigations_off_config(target: &str) -> String {
    format!("target.{target}.rustflags=[\"--cfg\",\"ferrix_mitigations_off\"]")
}

/// Where a kernel without its defences is built: a target directory of its
/// own, so that building one setting does not throw away the other's cache.
pub(crate) fn mitigations_off_target_dir() -> PathBuf {
    paths::target_dir().join("mitigations-off")
}

/// `cargo build -p ferrix-kernel` for `arch`, with the setting
/// [`set_mitigations`] chose, and where the ELF it makes will be.
fn kernel(arch: Arch, release: bool) -> Result<(Build, PathBuf)> {
    let target = arch.kernel_target();
    let build = build("ferrix-kernel", target, release)?;
    if !MITIGATIONS_OFF.load(Ordering::Relaxed) {
        return Ok((build, output(target, release, "ferrix-kernel")));
    }
    println!("  with --mitigations off: no side-channel defences");
    let directory = mitigations_off_target_dir();
    let profile = if release { "release" } else { "debug" };
    let made = directory.join(target).join(profile).join("ferrix-kernel");
    let build = build
        .args(["--config", &mitigations_off_config(target)])
        .args([std::ffi::OsStr::new("--target-dir"), directory.as_os_str()]);
    Ok((build, made))
}

/// Build the kernel for `arch` and return the ELF it produced.
pub(crate) fn build_kernel(arch: Arch, release: bool) -> Result<PathBuf> {
    let (build, made) = kernel(arch, release)?;
    build.output(&made).run()?;
    artifact(made)
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
    let profile = if release { "release" } else { "debug" };
    let path = target_dir
        .join(arch.kernel_target())
        .join(profile)
        .join(binary);
    build(package, arch.kernel_target(), release)?
        .env("CARGO_TARGET_DIR", target_dir)
        .output(&path)
        .run()?;
    artifact(path)
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
    let (build, made) = kernel(arch, release)?;
    build
        .input("FERRIX_INIT", init)
        .env("FERRIX_INIT_SCRIPT", script)
        .output(&made)
        .run()?;
    artifact(made)
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
    let (build, made) = kernel(arch, release)?;
    build
        .input("FERRIX_INIT_COMMANDS", commands)
        .output(&made)
        .run()?;
    artifact(made)
}

/// `cargo build -p <package> --target <target>`, for the caller to add to
/// and run: a [`Build`], which `FERRIX_BUILDS` may record or replay.
fn build(package: &str, target: &str, release: bool) -> Result<Build> {
    refuse_inherited_rustflags()?;
    println!("  building {package} for {target}");
    let build = Build::cargo(
        format!("cargo build -p {package} --target {target}"),
        paths::workspace_root(),
    )
    .args(["build", "--package", package, "--target", target]);
    Ok(if release {
        build.args(["--release"])
    } else {
        build
    })
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

/// Where cargo writes `name` for `target` in the target directory.
fn output(target: &str, release: bool, name: &str) -> PathBuf {
    let profile = if release { "release" } else { "debug" };
    paths::target_dir().join(target).join(profile).join(name)
}

/// `path`, checked for existence so that a rename in a manifest fails here
/// rather than as a confusing image error.
fn artifact(path: PathBuf) -> Result<PathBuf> {
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

    finished(status, description)
}

/// Turn a finished command's `status` into an error that names it, or into
/// nothing at all.
///
/// Apart from [`run`], which waits itself, a caller that had to spawn the
/// command to get at its output — `noise::run` — ends up here, so that a
/// failure reads the same whichever of them started it.
pub(crate) fn finished(status: std::process::ExitStatus, description: &str) -> Result<()> {
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
